use std::{
    cell::Cell,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
) -> Result<(), String> {
    let injected = REHEARSAL_PUBLICATION_FAULT.with(|cell| {
        if cell.get() == phase {
            cell.set(RehearsalPublicationFault::None);
            true
        } else {
            false
        }
    });
    if injected {
        Err(format!("injected rehearsal publication fault at {phase:?}"))
    } else {
        Ok(())
    }
}

const BACKUP_MANIFEST_SCHEMA_VERSION: u32 = 1;
const REHEARSAL_MARKER_SCHEMA_VERSION: u32 = 2;
const REHEARSAL_MARKER_FILENAME: &str = ".tracedecay-profile-rehearsal.json";
const REQUIRED_PROFILE_PATHS: &[&str] = &[
    "global.db",
    "global.db-wal",
    "global.db-shm",
    "user-sessions.db",
    "user-sessions.db-wal",
    "user-sessions.db-shm",
    "user-memory.db",
    "user-memory.db-wal",
    "user-memory.db-shm",
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
    pub source_profile_identity_sha256: Option<String>,
    pub entries: Vec<ProfileBackupEntry>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct ProfileBackupRehearsalMarker {
    schema_version: u32,
    backup_id: String,
    backup_root: PathBuf,
    manifest_sha256: String,
    source_profile_identity_sha256: Option<String>,
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
    lifecycle: &crate::lifecycle_lease::LifecycleLease,
) -> Result<PathBuf, String> {
    if backup_id.is_empty() || created_at <= 0 {
        return Err("backup identity and timestamp must be non-empty".to_owned());
    }
    let source = fs::canonicalize(profile_root)
        .map_err(|error| format!("canonicalize profile '{}': {error}", profile_root.display()))?;
    if !lifecycle.is_exclusive() || !lifecycle.guards_profile(&source) {
        return Err(
            "complete profile backup requires the exact exclusive profile lease".to_owned(),
        );
    }
    fs::create_dir_all(backup_parent).map_err(|error| {
        format!(
            "create backup parent '{}': {error}",
            backup_parent.display()
        )
    })?;
    let parent = fs::canonicalize(backup_parent).map_err(|error| {
        format!(
            "canonicalize backup parent '{}': {error}",
            backup_parent.display()
        )
    })?;
    if parent.starts_with(&source) {
        return Err("backup destination must be outside the source profile".to_owned());
    }

    let final_root = parent.join(backup_id);
    let staging = parent.join(format!(".{backup_id}.tmp"));
    if final_root.exists() || staging.exists() {
        return Err("backup destination already exists".to_owned());
    }
    fs::create_dir(&staging)
        .map_err(|error| format!("create backup staging '{}': {error}", staging.display()))?;

    let result = create_backup_contents(&source, &staging, backup_id, created_at);
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    fs::rename(&staging, &final_root).map_err(|error| {
        format!(
            "publish backup '{}' to '{}': {error}",
            staging.display(),
            final_root.display()
        )
    })?;
    sync_directory(&parent)?;
    Ok(final_root)
}

pub fn rehearse_complete_profile_backup(
    backup_root: &Path,
    restore_root: &Path,
) -> Result<CompleteProfileBackupManifest, String> {
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
        return Err("restore destination must not already exist".to_owned());
    }
    fs::create_dir(&staging).map_err(|error| {
        format!(
            "create restore staging directory '{}': {error}",
            staging.display()
        )
    })?;
    restrict_private_directory(&staging)?;
    let marker_path = staging.join(REHEARSAL_MARKER_FILENAME);
    write_new_synced(
        &marker_path,
        &serde_json::to_vec_pretty(&marker)
            .map_err(|error| format!("encode profile rehearsal marker: {error}"))?,
    )?;
    let result = (|| {
        for entry in backup.manifest.entries.iter().filter(|entry| entry.present) {
            let source = checked_join(&backup.root, &entry.logical_path)?;
            let destination = checked_join(&staging, &entry.logical_path)?;
            copy_verified_file(&source, &destination, entry)?;
        }
        let restored = verify_restored_copy(&staging, &backup.manifest)?;
        if restored != backup.manifest {
            return Err("restored profile inventory differs from backup manifest".to_owned());
        }
        rebind_restored_store_manifests(&staging, &restore_root)?;
        verify_restored_rehearsal(&staging, &restore_root, &backup)?;
        // Keep the ownership marker through rename and parent sync so a crash
        // never leaves an unowned staging directory or an unmarked published
        // root that recovery cannot finish.
        sync_directory(&staging)?;
        inject_rehearsal_publication_fault(RehearsalPublicationFault::BeforeRename)?;
        fs::rename(&staging, &restore_root).map_err(|error| {
            format!(
                "publish rehearsed profile '{}' to '{}': {error}",
                staging.display(),
                restore_root.display()
            )
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

fn absolute_destination(path: &Path) -> Result<PathBuf, String> {
    let name = path
        .file_name()
        .ok_or_else(|| "restore destination must name a directory".to_owned())?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = parent.canonicalize().map_err(|error| {
        format!(
            "canonicalize restore destination parent '{}': {error}",
            parent.display()
        )
    })?;
    Ok(parent.join(name))
}

fn rehearsal_staging_path(restore_root: &Path) -> Result<PathBuf, String> {
    let parent = restore_root
        .parent()
        .ok_or_else(|| "restore destination has no parent".to_owned())?;
    let name = restore_root
        .file_name()
        .ok_or_else(|| "restore destination must name a directory".to_owned())?
        .to_string_lossy();
    Ok(parent.join(format!(".{name}.tracedecay-rehearsal")))
}

fn recover_interrupted_publication(
    restore_root: &Path,
    expected_marker: &ProfileBackupRehearsalMarker,
    backup: &VerifiedCompleteProfileBackup,
) -> Result<bool, String> {
    let metadata = match fs::symlink_metadata(restore_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "inspect restore destination '{}': {error}",
                restore_root.display()
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Ok(false);
    }
    let marker_path = restore_root.join(REHEARSAL_MARKER_FILENAME);
    match fs::symlink_metadata(&marker_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "inspect published rehearsal marker '{}': {error}",
                marker_path.display()
            ));
        }
        Ok(marker_metadata) => {
            if marker_metadata.file_type().is_symlink() || !marker_metadata.is_file() {
                return Err(format!(
                    "published rehearsal marker '{}' is not a regular file",
                    marker_path.display()
                ));
            }
        }
    }
    let marker = read_rehearsal_marker(&marker_path)?;
    if marker != *expected_marker {
        return Err(format!(
            "published rehearsal root '{}' belongs to another restore attempt",
            restore_root.display()
        ));
    }
    verify_restored_rehearsal(restore_root, restore_root, backup)?;
    finish_published_rehearsal(restore_root, expected_marker)?;
    Ok(true)
}

fn recover_interrupted_staging(
    staging: &Path,
    expected_marker: &ProfileBackupRehearsalMarker,
) -> Result<(), String> {
    let metadata = match fs::symlink_metadata(staging) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "inspect profile rehearsal staging '{}': {error}",
                staging.display()
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "profile rehearsal staging '{}' is not an owned directory",
            staging.display()
        ));
    }
    let marker_path = staging.join(REHEARSAL_MARKER_FILENAME);
    let marker_metadata = fs::symlink_metadata(&marker_path).map_err(|error| {
        format!(
            "interrupted profile rehearsal staging '{}' has no readable ownership marker: {error}",
            staging.display()
        )
    })?;
    if marker_metadata.file_type().is_symlink() || !marker_metadata.is_file() {
        return Err(format!(
            "profile rehearsal marker '{}' is not a regular file",
            marker_path.display()
        ));
    }
    let marker = read_rehearsal_marker(&marker_path)?;
    if marker != *expected_marker {
        return Err(format!(
            "profile rehearsal staging '{}' belongs to another restore attempt",
            staging.display()
        ));
    }
    fs::remove_dir_all(staging).map_err(|error| {
        format!(
            "clear interrupted profile rehearsal staging '{}': {error}",
            staging.display()
        )
    })?;
    sync_directory(
        staging
            .parent()
            .ok_or_else(|| "profile rehearsal staging has no parent".to_owned())?,
    )
}

fn finish_published_rehearsal(
    restore_root: &Path,
    expected_marker: &ProfileBackupRehearsalMarker,
) -> Result<(), String> {
    let marker_path = restore_root.join(REHEARSAL_MARKER_FILENAME);
    let marker = read_rehearsal_marker(&marker_path)?;
    if marker != *expected_marker {
        return Err(format!(
            "published rehearsal root '{}' belongs to another restore attempt",
            restore_root.display()
        ));
    }
    let parent = restore_root
        .parent()
        .ok_or_else(|| "restore destination has no parent".to_owned())?;
    sync_directory(parent)?;
    inject_rehearsal_publication_fault(
        RehearsalPublicationFault::AfterParentSyncBeforeMarkerRemoval,
    )?;
    fs::remove_file(&marker_path).map_err(|error| {
        format!(
            "remove settled profile rehearsal marker '{}': {error}",
            marker_path.display()
        )
    })?;
    sync_directory(restore_root)?;
    sync_directory(parent)
}

fn read_rehearsal_marker(marker_path: &Path) -> Result<ProfileBackupRehearsalMarker, String> {
    serde_json::from_slice(&fs::read(marker_path).map_err(|error| {
        format!(
            "read profile rehearsal marker '{}': {error}",
            marker_path.display()
        )
    })?)
    .map_err(|error| {
        format!(
            "decode profile rehearsal marker '{}': {error}",
            marker_path.display()
        )
    })
}

fn rebind_restored_store_manifests(
    staged_profile_root: &Path,
    published_profile_root: &Path,
) -> Result<(), String> {
    let projects = staged_profile_root.join("projects");
    let metadata = match fs::symlink_metadata(&projects) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "inspect restored project stores '{}': {error}",
                projects.display()
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "restored project stores '{}' must be a regular directory",
            projects.display()
        ));
    }
    let mut stores = fs::read_dir(&projects)
        .map_err(|error| format!("read restored project stores: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("read restored project store entry: {error}"))?;
    stores.sort_by_key(fs::DirEntry::file_name);
    for store in stores {
        let store_root = store.path();
        let metadata = fs::symlink_metadata(&store_root).map_err(|error| {
            format!("inspect restored store '{}': {error}", store_root.display())
        })?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "restored store '{}' must not be a symlink",
                store_root.display()
            ));
        }
        if !metadata.is_dir() {
            continue;
        }
        let manifest_path = store_root.join(crate::storage::STORE_MANIFEST_FILENAME);
        let manifest_metadata = match fs::symlink_metadata(&manifest_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(format!(
                    "restored store '{}' is missing required {}",
                    store_root.display(),
                    crate::storage::STORE_MANIFEST_FILENAME
                ));
            }
            Err(error) => {
                return Err(format!(
                    "inspect restored store manifest '{}': {error}",
                    manifest_path.display()
                ));
            }
        };
        if manifest_metadata.file_type().is_symlink() || !manifest_metadata.is_file() {
            return Err(format!(
                "restored store manifest '{}' must be a regular file",
                manifest_path.display()
            ));
        }
        let manifest = crate::storage::read_store_manifest(&manifest_path)
            .map_err(|error| error.to_string())?;
        let project_id = store
            .file_name()
            .into_string()
            .map_err(|_| "restored project store id is not Unicode".to_owned())?;
        let manifest = rebound_store_manifest(
            manifest,
            &project_id,
            published_profile_root,
            &manifest_path,
        )?;
        crate::storage::write_store_manifest_to_path(&manifest_path, &manifest)
            .map_err(|error| error.to_string())?;
        if crate::storage::read_store_manifest(&manifest_path).map_err(|error| error.to_string())?
            != manifest
        {
            return Err(format!(
                "restored store manifest '{}' changed during rebinding",
                manifest_path.display()
            ));
        }
    }
    Ok(())
}

fn rebound_store_manifest(
    mut manifest: crate::storage::StoreManifest,
    project_id: &str,
    published_profile_root: &Path,
    manifest_path: &Path,
) -> Result<crate::storage::StoreManifest, String> {
    if manifest.schema_version != crate::storage::STORE_MANIFEST_SCHEMA_VERSION
        || manifest.project_id.as_deref() != Some(project_id)
        || manifest.store_kind != crate::storage::StoreKind::CodeProject
        || manifest.storage_mode != crate::storage::StorageMode::ProfileSharded
    {
        return Err(format!(
            "restored store manifest '{}' does not match its enrollment",
            manifest_path.display()
        ));
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
) -> Result<(), String> {
    for entry in backup.manifest.entries.iter().filter(|entry| entry.present) {
        if restored_store_manifest_project_id(&entry.logical_path).is_none() {
            verify_file(
                &checked_join(restored_profile_root, &entry.logical_path)?,
                entry,
            )?;
        }
    }
    for entry in backup.manifest.entries.iter().filter(|entry| entry.present) {
        let Some(project_id) = restored_store_manifest_project_id(&entry.logical_path) else {
            continue;
        };
        let source_path = checked_join(&backup.root, &entry.logical_path)?;
        let source_manifest =
            crate::storage::read_store_manifest(&source_path).map_err(|error| error.to_string())?;
        let expected = rebound_store_manifest(
            source_manifest,
            project_id,
            published_profile_root,
            &source_path,
        )?;
        let restored_path = checked_join(restored_profile_root, &entry.logical_path)?;
        let restored = crate::storage::read_store_manifest(&restored_path)
            .map_err(|error| error.to_string())?;
        if restored != expected {
            return Err(format!(
                "restored store manifest '{}' does not match its rebound backup manifest",
                restored_path.display()
            ));
        }
    }
    Ok(())
}

fn restored_store_manifest_project_id(logical_path: &str) -> Option<&str> {
    let mut components = logical_path.split('/');
    match (
        components.next(),
        components.next(),
        components.next(),
        components.next(),
    ) {
        (
            Some("projects"),
            Some(project_id),
            Some(crate::storage::STORE_MANIFEST_FILENAME),
            None,
        ) if !project_id.is_empty() => Some(project_id),
        _ => None,
    }
}

fn validate_restored_store_relative_path(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(format!(
            "restored store manifest contains unsafe relative path '{}'",
            path.display()
        ));
    }
    Ok(())
}

pub fn load_and_verify_backup(backup_root: &Path) -> Result<CompleteProfileBackupManifest, String> {
    Ok(load_verified_backup(backup_root)?.manifest)
}

fn load_verified_backup(backup_root: &Path) -> Result<VerifiedCompleteProfileBackup, String> {
    let root = fs::canonicalize(backup_root)
        .map_err(|error| format!("canonicalize backup '{}': {error}", backup_root.display()))?;
    let metadata = fs::symlink_metadata(&root)
        .map_err(|error| format!("inspect backup root '{}': {error}", root.display()))?;
    if !metadata.is_dir() {
        return Err(format!(
            "complete-profile backup root '{}' is not a directory",
            root.display()
        ));
    }
    let manifest_path = root.join("backup-manifest.json");
    let bytes = fs::read(&manifest_path).map_err(|error| {
        format!(
            "read backup manifest '{}': {error}",
            manifest_path.display()
        )
    })?;
    let manifest: CompleteProfileBackupManifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("decode backup manifest: {error}"))?;
    validate_manifest(&manifest)?;
    for entry in manifest.entries.iter().filter(|entry| entry.present) {
        let path = checked_join(&root, &entry.logical_path)?;
        verify_file(&path, entry)?;
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
) -> Result<(), String> {
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
    for entry in entries.iter().filter(|entry| entry.present) {
        let source_path = checked_join(source, &entry.logical_path)?;
        let destination = checked_join(staging, &entry.logical_path)?;
        copy_verified_file(&source_path, &destination, entry)?;
    }

    let profile_identity = source.join("profile-identity.json");
    let manifest = CompleteProfileBackupManifest {
        schema_version: BACKUP_MANIFEST_SCHEMA_VERSION,
        backup_id: backup_id.to_owned(),
        created_at,
        source_profile_identity_sha256: profile_identity
            .is_file()
            .then(|| sha256_file(&profile_identity))
            .transpose()?,
        entries,
    };
    validate_manifest(&manifest)?;
    let manifest_path = staging.join("backup-manifest.json");
    let bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("encode backup manifest: {error}"))?;
    write_new_synced(&manifest_path, &bytes)?;
    sync_directory(staging)
}

fn collect_files(
    root: &Path,
    path: &Path,
    entries: &mut Vec<ProfileBackupEntry>,
) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("inspect backup source '{}': {error}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!("backup source is a symlink: '{}'", path.display()));
    }
    if metadata.is_dir() {
        let mut children = fs::read_dir(path)
            .map_err(|error| format!("read backup directory '{}': {error}", path.display()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("read backup directory '{}': {error}", path.display()))?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            collect_files(root, &child.path(), entries)?;
        }
        return Ok(());
    }
    if !metadata.is_file() {
        return Err(format!(
            "backup source is not a regular file: '{}'",
            path.display()
        ));
    }
    let logical = path
        .strip_prefix(root)
        .map_err(|_| "backup source escaped profile root".to_owned())?
        .to_string_lossy()
        .replace('\\', "/");
    entries.push(ProfileBackupEntry {
        logical_path: logical,
        present: true,
        byte_len: Some(metadata.len()),
        sha256: Some(sha256_file(path)?),
    });
    Ok(())
}

fn verify_restored_copy(
    restore_root: &Path,
    expected: &CompleteProfileBackupManifest,
) -> Result<CompleteProfileBackupManifest, String> {
    for entry in expected.entries.iter().filter(|entry| entry.present) {
        verify_file(&checked_join(restore_root, &entry.logical_path)?, entry)?;
    }
    Ok(expected.clone())
}

fn copy_verified_file(
    source: &Path,
    destination: &Path,
    expected: &ProfileBackupEntry,
) -> Result<(), String> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "create backup destination directory '{}': {error}",
                parent.display()
            )
        })?;
    }
    fs::copy(source, destination).map_err(|error| {
        format!(
            "copy backup file '{}' to '{}': {error}",
            source.display(),
            destination.display()
        )
    })?;
    sync_file(destination)?;
    verify_file(destination, expected)
}

fn verify_file(path: &Path, expected: &ProfileBackupEntry) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("inspect backup file '{}': {error}", path.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!(
            "backup artifact is not a regular file: '{}'",
            path.display()
        ));
    }
    if Some(metadata.len()) != expected.byte_len || Some(sha256_file(path)?) != expected.sha256 {
        return Err(format!(
            "backup artifact checksum mismatch: '{}'",
            path.display()
        ));
    }
    Ok(())
}

fn validate_manifest(manifest: &CompleteProfileBackupManifest) -> Result<(), String> {
    if manifest.schema_version != BACKUP_MANIFEST_SCHEMA_VERSION
        || manifest.backup_id.is_empty()
        || manifest.created_at <= 0
    {
        return Err("invalid complete-profile backup manifest identity".to_owned());
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
            return Err("invalid complete-profile backup manifest entry".to_owned());
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
            return Err(format!("backup manifest omits required path '{required}'"));
        }
    }
    let profile_identity_sha256 = manifest
        .entries
        .iter()
        .find(|entry| entry.logical_path == "profile-identity.json" && entry.present)
        .and_then(|entry| entry.sha256.clone());
    if manifest.source_profile_identity_sha256 != profile_identity_sha256 {
        return Err(
            "backup manifest source profile identity digest does not match content".to_owned(),
        );
    }
    Ok(())
}

fn checked_join(root: &Path, logical: &str) -> Result<PathBuf, String> {
    let path = Path::new(logical);
    if path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err("backup manifest path is not relative".to_owned());
    }
    Ok(root.join(path))
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path)
        .map_err(|error| format!("open '{}' for hashing: {error}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("hash '{}': {error}", path.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn write_new_synced(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| format!("create '{}': {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("write '{}': {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("sync '{}': {error}", path.display()))
}

fn sync_file(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("sync '{}': {error}", path.display()))
}

fn sync_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        File::open(path)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("sync directory '{}': {error}", path.display()))?;
    }
    Ok(())
}

#[cfg(unix)]
fn restrict_private_directory(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("restrict directory '{}': {error}", path.display()))
}

#[cfg(not(unix))]
fn restrict_private_directory(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn released_profile(root: &Path) {
        for name in [
            "global.db",
            "global.db-wal",
            "global.db-shm",
            "user-sessions.db",
            "user-sessions.db-wal",
            "user-sessions.db-shm",
            "user-memory.db",
            "user-memory.db-wal",
            "user-memory.db-shm",
            "enrollment.json",
            "config.toml",
            "profile-identity.json",
        ] {
            let path = root.join(name);
            fs::write(&path, format!("released fixture: {name}")).unwrap();
        }
        fs::create_dir(root.join("projects")).unwrap();
        fs::write(
            root.join("projects/project.release.db"),
            b"project schema 18",
        )
        .unwrap();
        fs::create_dir(root.join("migration-inventory")).unwrap();
        fs::write(
            root.join("migration-inventory/migration.release.json"),
            b"migration inventory",
        )
        .unwrap();
    }

    #[test]
    fn complete_backup_rehearses_from_restored_isolated_copy() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("profile");
        let backups = temp.path().join("backups");
        let restore = temp.path().join("restored");
        fs::create_dir(&profile).unwrap();
        released_profile(&profile);
        let lease =
            crate::lifecycle_lease::acquire_exclusive_for_profile(&profile, "backup test").unwrap();

        let backup =
            create_complete_profile_backup(&profile, &backups, "backup.release", 100, &lease)
                .unwrap();
        let manifest = rehearse_complete_profile_backup(&backup, &restore).unwrap();

        assert!(
            manifest
                .entries
                .iter()
                .any(|entry| { entry.logical_path == "user-sessions.db-wal" && entry.present })
        );
        assert!(
            manifest.entries.iter().any(|entry| {
                entry.logical_path == "projects/project.release.db" && entry.present
            })
        );
        assert_eq!(
            fs::read(restore.join("user-memory.db")).unwrap(),
            fs::read(profile.join("user-memory.db")).unwrap()
        );
    }

    #[test]
    fn rehearsal_rebinds_relocated_store_without_changing_durable_identity() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("released-profile");
        let backups = temp.path().join("backups");
        let restore = temp.path().join("rehearsed-profile");
        let project = temp.path().join("released-project");
        let project_id = "project.release";
        fs::create_dir(&profile).unwrap();
        fs::create_dir(&project).unwrap();
        released_profile(&profile);
        fs::remove_file(profile.join("projects/project.release.db")).unwrap();
        let source_store = profile.join("projects").join(project_id);
        fs::create_dir(&source_store).unwrap();
        for (name, contents) in [
            ("tracedecay.db", b"released memory identity".as_slice()),
            ("sessions.db", b"released LCM identity".as_slice()),
            (
                "branch-meta.json",
                br#"{"default_branch":"main","branches":{}}"#,
            ),
        ] {
            fs::write(source_store.join(name), contents).unwrap();
        }
        let source_manifest = crate::storage::StoreManifest {
            schema_version: crate::storage::STORE_MANIFEST_SCHEMA_VERSION,
            project_id: Some(project_id.to_owned()),
            store_kind: crate::storage::StoreKind::CodeProject,
            storage_mode: crate::storage::StorageMode::ProfileSharded,
            project_root: project.clone(),
            data_root: source_store.clone(),
            graph_db_relpath: "tracedecay.db".into(),
            sessions_db_relpath: "sessions.db".into(),
            branch_meta_relpath: "branch-meta.json".into(),
        };
        crate::storage::write_store_manifest_to_path(
            &source_store.join(crate::storage::STORE_MANIFEST_FILENAME),
            &source_manifest,
        )
        .unwrap();
        let expected_profile_identity = fs::read(profile.join("profile-identity.json")).unwrap();
        let expected_memory = fs::read(profile.join("user-memory.db")).unwrap();
        let expected_lcm = fs::read(profile.join("user-sessions.db")).unwrap();
        let expected_config = fs::read(profile.join("config.toml")).unwrap();
        let lease =
            crate::lifecycle_lease::acquire_exclusive_for_profile(&profile, "backup test").unwrap();

        let backup =
            create_complete_profile_backup(&profile, &backups, "backup.release", 100, &lease)
                .unwrap();
        rehearse_complete_profile_backup(&backup, &restore).unwrap();

        let restored_store = restore.join("projects").join(project_id);
        let restored_manifest = crate::storage::read_store_manifest(
            &restored_store.join(crate::storage::STORE_MANIFEST_FILENAME),
        )
        .unwrap();
        assert_eq!(restored_manifest.project_id.as_deref(), Some(project_id));
        assert_eq!(restored_manifest.project_root, project);
        assert_eq!(
            restored_manifest.data_root,
            restored_store.canonicalize().unwrap()
        );
        assert_eq!(
            crate::storage::read_store_manifest(
                &source_store.join(crate::storage::STORE_MANIFEST_FILENAME)
            )
            .unwrap(),
            source_manifest,
            "rehearsal must never mutate the source fixture profile"
        );
        assert_eq!(
            fs::read(restore.join("profile-identity.json")).unwrap(),
            expected_profile_identity
        );
        assert_eq!(
            fs::read(restore.join("user-memory.db")).unwrap(),
            expected_memory
        );
        assert_eq!(
            fs::read(restore.join("user-sessions.db")).unwrap(),
            expected_lcm
        );
        assert_eq!(
            fs::read(restore.join("config.toml")).unwrap(),
            expected_config
        );
    }

    #[test]
    fn rehearsal_rejects_corrupted_backup_material() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("profile");
        let backups = temp.path().join("backups");
        fs::create_dir(&profile).unwrap();
        released_profile(&profile);
        let lease =
            crate::lifecycle_lease::acquire_exclusive_for_profile(&profile, "backup test").unwrap();
        let backup =
            create_complete_profile_backup(&profile, &backups, "backup.release", 100, &lease)
                .unwrap();
        fs::write(backup.join("global.db"), b"corrupt").unwrap();

        let error =
            rehearse_complete_profile_backup(&backup, &temp.path().join("restored")).unwrap_err();
        assert!(error.contains("checksum mismatch"));
    }

    #[test]
    fn backup_refuses_destination_inside_live_profile() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("profile");
        fs::create_dir(&profile).unwrap();
        released_profile(&profile);
        let lease =
            crate::lifecycle_lease::acquire_exclusive_for_profile(&profile, "backup test").unwrap();

        let error = create_complete_profile_backup(
            &profile,
            &profile.join("backups"),
            "backup.release",
            100,
            &lease,
        )
        .unwrap_err();
        assert!(error.contains("outside the source profile"));
    }

    fn sharded_released_backup(temp: &tempfile::TempDir) -> (PathBuf, PathBuf) {
        let profile = temp.path().join("released-profile");
        let backups = temp.path().join("backups");
        let project = temp.path().join("released-project");
        let project_id = "project.release";
        fs::create_dir(&profile).unwrap();
        fs::create_dir(&project).unwrap();
        released_profile(&profile);
        fs::remove_file(profile.join("projects/project.release.db")).unwrap();
        let source_store = profile.join("projects").join(project_id);
        fs::create_dir(&source_store).unwrap();
        for (name, contents) in [
            ("tracedecay.db", b"released memory identity".as_slice()),
            ("sessions.db", b"released LCM identity".as_slice()),
            (
                "branch-meta.json",
                br#"{"default_branch":"main","branches":{}}"#,
            ),
        ] {
            fs::write(source_store.join(name), contents).unwrap();
        }
        crate::storage::write_store_manifest_to_path(
            &source_store.join(crate::storage::STORE_MANIFEST_FILENAME),
            &crate::storage::StoreManifest {
                schema_version: crate::storage::STORE_MANIFEST_SCHEMA_VERSION,
                project_id: Some(project_id.to_owned()),
                store_kind: crate::storage::StoreKind::CodeProject,
                storage_mode: crate::storage::StorageMode::ProfileSharded,
                project_root: project,
                data_root: source_store.clone(),
                graph_db_relpath: "tracedecay.db".into(),
                sessions_db_relpath: "sessions.db".into(),
                branch_meta_relpath: "branch-meta.json".into(),
            },
        )
        .unwrap();
        let lease =
            crate::lifecycle_lease::acquire_exclusive_for_profile(&profile, "backup test").unwrap();
        let backup =
            create_complete_profile_backup(&profile, &backups, "backup.release", 100, &lease)
                .unwrap();
        (backup, temp.path().join("rehearsed-profile"))
    }

    #[test]
    fn rehearsal_publication_faults_resume_or_rollback_at_each_boundary() {
        for (fault, expect_staging, expect_published_marker) in [
            ("before_rename", true, false),
            ("after_rename_before_parent_sync", false, true),
            ("after_parent_sync_before_marker_removal", false, true),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let (backup, restore) = sharded_released_backup(&temp);
            let staging = temp.path().join(".rehearsed-profile.tracedecay-rehearsal");
            set_rehearsal_publication_fault_for_test(fault);

            let error = rehearse_complete_profile_backup(&backup, &restore).unwrap_err();
            assert!(
                error.contains("injected rehearsal publication fault"),
                "{fault}: unexpected error {error}"
            );
            assert_eq!(
                staging.is_dir(),
                expect_staging,
                "{fault}: staging presence"
            );
            assert_eq!(
                restore.join(REHEARSAL_MARKER_FILENAME).is_file(),
                expect_published_marker,
                "{fault}: published marker presence"
            );

            set_rehearsal_publication_fault_for_test("");
            rehearse_complete_profile_backup(&backup, &restore).unwrap();
            assert!(restore.join("profile-identity.json").is_file());
            assert!(!staging.exists());
            assert!(!restore.join(REHEARSAL_MARKER_FILENAME).exists());
        }
    }

    #[test]
    fn rehearsal_rejects_project_store_missing_store_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let (backup, restore) = sharded_released_backup(&temp);
        let manifest_entry = format!(
            "projects/project.release/{}",
            crate::storage::STORE_MANIFEST_FILENAME
        );
        fs::remove_file(backup.join(&manifest_entry)).unwrap();
        let manifest_path = backup.join("backup-manifest.json");
        let mut manifest: CompleteProfileBackupManifest =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest
            .entries
            .retain(|entry| entry.logical_path != manifest_entry);
        fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();

        let error = rehearse_complete_profile_backup(&backup, &restore).unwrap_err();
        assert!(
            error.contains("missing required store_manifest.json"),
            "unexpected error: {error}"
        );
        assert!(!restore.exists());
    }
}
