use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const BACKUP_MANIFEST_SCHEMA_VERSION: u32 = 1;
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
    if restore_root.exists() {
        return Err("restore destination must not already exist".to_owned());
    }
    let manifest = load_and_verify_backup(backup_root)?;
    fs::create_dir(restore_root).map_err(|error| {
        format!(
            "create restore destination '{}': {error}",
            restore_root.display()
        )
    })?;
    let result = (|| {
        for entry in manifest.entries.iter().filter(|entry| entry.present) {
            let source = checked_join(backup_root, &entry.logical_path)?;
            let destination = checked_join(restore_root, &entry.logical_path)?;
            copy_verified_file(&source, &destination, entry)?;
        }
        let restored = verify_restored_copy(restore_root, &manifest)?;
        if restored != manifest {
            return Err("restored profile inventory differs from backup manifest".to_owned());
        }
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_dir_all(restore_root);
        return Err(error);
    }
    Ok(manifest)
}

pub fn load_and_verify_backup(backup_root: &Path) -> Result<CompleteProfileBackupManifest, String> {
    let manifest_path = backup_root.join("backup-manifest.json");
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
        let path = checked_join(backup_root, &entry.logical_path)?;
        verify_file(&path, entry)?;
    }
    Ok(manifest)
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
}
