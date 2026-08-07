use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tracedecay::profile_backup::{
    ProfileBackupError, create_complete_profile_backup, rehearse_complete_profile_backup,
    set_rehearsal_publication_fault_for_test,
};
use tracedecay::storage::{
    STORE_MANIFEST_FILENAME, STORE_MANIFEST_SCHEMA_VERSION, StorageMode, StoreKind, StoreManifest,
    read_store_manifest, write_store_manifest_to_path,
};

struct ReleasedProfileFixture {
    profile: PathBuf,
    project: PathBuf,
    project_id: &'static str,
    source_store: PathBuf,
}

fn write_profile_identity(profile: &Path, brain_id: &str, profile_id: &str) {
    let path = profile.join("profile-identity.json");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "brain_id": brain_id,
            "profile_id": profile_id,
        }))
        .unwrap(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn seed_released_profile(temp: &TempDir) -> ReleasedProfileFixture {
    let profile = temp.path().join("released-profile");
    let project = temp.path().join("released-project");
    let project_id = "project.release";
    let source_store = profile.join("projects").join(project_id);
    fs::create_dir_all(&source_store).unwrap();
    fs::create_dir(&project).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(&profile, fs::Permissions::from_mode(0o700)).unwrap();
    }
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
    ] {
        fs::write(profile.join(name), format!("released fixture: {name}")).unwrap();
    }
    write_profile_identity(&profile, "brain.release", "profile.release");
    fs::create_dir(profile.join("migration-inventory")).unwrap();
    fs::write(
        profile.join("migration-inventory/released.json"),
        b"released migration inventory",
    )
    .unwrap();
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
    write_store_manifest_to_path(
        &source_store.join(STORE_MANIFEST_FILENAME),
        &StoreManifest {
            schema_version: STORE_MANIFEST_SCHEMA_VERSION,
            project_id: Some(project_id.to_owned()),
            store_kind: StoreKind::CodeProject,
            storage_mode: StorageMode::ProfileSharded,
            project_root: project.clone(),
            data_root: source_store.clone(),
            graph_db_relpath: "tracedecay.db".into(),
            sessions_db_relpath: "sessions.db".into(),
            branch_meta_relpath: "branch-meta.json".into(),
        },
    )
    .unwrap();
    ReleasedProfileFixture {
        profile,
        project,
        project_id,
        source_store,
    }
}

fn create_backup(temp: &TempDir, fixture: &ReleasedProfileFixture) -> PathBuf {
    let lease = tracedecay::lifecycle_lease::acquire_exclusive_for_profile(
        &fixture.profile,
        "released-copy rehearsal test",
    )
    .unwrap();
    create_complete_profile_backup(
        &fixture.profile,
        &temp.path().join("backups"),
        "backup.release",
        100,
        &lease,
    )
    .unwrap()
}

fn read_bytes(root: &Path, relative: &str) -> Vec<u8> {
    fs::read(root.join(relative)).unwrap()
}

#[test]
fn released_copy_rehearsal_rebinds_store_and_preserves_identity() {
    let temp = TempDir::new().unwrap();
    let fixture = seed_released_profile(&temp);
    let backup = create_backup(&temp, &fixture);
    let restore = temp.path().join("rehearsed-profile");
    let expected_profile_identity = read_bytes(&fixture.profile, "profile-identity.json");
    let expected_memory = read_bytes(&fixture.profile, "user-memory.db");
    let expected_lcm = read_bytes(&fixture.profile, "user-sessions.db");
    let expected_config = read_bytes(&fixture.profile, "config.toml");

    rehearse_complete_profile_backup(&backup, &restore).unwrap();

    let restored_store = restore.join("projects").join(fixture.project_id);
    let restored_manifest =
        read_store_manifest(&restored_store.join(STORE_MANIFEST_FILENAME)).unwrap();
    assert_eq!(
        restored_manifest.project_id.as_deref(),
        Some(fixture.project_id)
    );
    assert_eq!(restored_manifest.project_root, fixture.project);
    assert_eq!(
        restored_manifest.data_root,
        restored_store.canonicalize().unwrap()
    );
    assert_eq!(
        read_store_manifest(&fixture.source_store.join(STORE_MANIFEST_FILENAME))
            .unwrap()
            .data_root,
        fixture.source_store
    );
    assert_eq!(
        read_bytes(&restore, "profile-identity.json"),
        expected_profile_identity
    );
    assert_eq!(read_bytes(&restore, "user-memory.db"), expected_memory);
    assert_eq!(read_bytes(&restore, "user-sessions.db"), expected_lcm);
    assert_eq!(read_bytes(&restore, "config.toml"), expected_config);
    // Database sidecars are folded into snapshot artifacts, never restored
    // as standalone files.
    assert!(!restore.join("user-memory.db-wal").exists());
    assert!(!restore.join("global.db-shm").exists());
}

#[test]
fn released_copy_rehearsal_recovers_owned_interrupted_staging() {
    let temp = TempDir::new().unwrap();
    let fixture = seed_released_profile(&temp);
    let backup = create_backup(&temp, &fixture);
    let restore = temp.path().join("rehearsed-profile");
    let staging = temp.path().join(".rehearsed-profile.tracedecay-rehearsal");
    set_rehearsal_publication_fault_for_test("before_rename");

    let error = rehearse_complete_profile_backup(&backup, &restore).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("injected rehearsal publication fault")
    );
    assert!(staging.join(".tracedecay-profile-rehearsal.json").is_file());

    rehearse_complete_profile_backup(&backup, &restore).unwrap();

    assert!(restore.join("profile-identity.json").is_file());
    assert!(!staging.exists());
}

#[test]
fn released_copy_rehearsal_rejects_missing_store_manifest() {
    let temp = TempDir::new().unwrap();
    let fixture = seed_released_profile(&temp);
    let backup = create_backup(&temp, &fixture);
    let restore = temp.path().join("rehearsed-profile");
    let manifest_entry = format!(
        "projects/{}/{}",
        fixture.project_id, STORE_MANIFEST_FILENAME
    );
    fs::remove_file(backup.join(&manifest_entry)).unwrap();
    let manifest_path = backup.join("backup-manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    let entries = manifest
        .get_mut("entries")
        .and_then(|value| value.as_array_mut())
        .unwrap();
    entries.retain(|entry| {
        entry.get("logical_path").and_then(|value| value.as_str()) != Some(manifest_entry.as_str())
    });
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let error = rehearse_complete_profile_backup(&backup, &restore).unwrap_err();
    assert!(
        matches!(&error, ProfileBackupError::CorruptBackup { message }
            if message.contains("missing required store_manifest.json")),
        "unexpected error: {error}"
    );
    assert!(!restore.exists());
}

#[test]
fn released_copy_rehearsal_resumes_after_rename_before_marker_removal() {
    let temp = TempDir::new().unwrap();
    let fixture = seed_released_profile(&temp);
    let backup = create_backup(&temp, &fixture);
    let restore = temp.path().join("rehearsed-profile");
    set_rehearsal_publication_fault_for_test("after_rename_before_parent_sync");

    let error = rehearse_complete_profile_backup(&backup, &restore).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("injected rehearsal publication fault")
    );
    assert!(restore.join(".tracedecay-profile-rehearsal.json").is_file());

    set_rehearsal_publication_fault_for_test("");
    rehearse_complete_profile_backup(&backup, &restore).unwrap();
    assert!(restore.join("profile-identity.json").is_file());
    assert!(!restore.join(".tracedecay-profile-rehearsal.json").exists());
}

#[test]
fn marked_publication_recovery_rejects_tampered_durable_bytes() {
    let temp = TempDir::new().unwrap();
    let fixture = seed_released_profile(&temp);
    let backup = create_backup(&temp, &fixture);
    let restore = temp.path().join("rehearsed-profile");
    let marker = restore.join(".tracedecay-profile-rehearsal.json");
    set_rehearsal_publication_fault_for_test("after_rename_before_parent_sync");
    rehearse_complete_profile_backup(&backup, &restore).unwrap_err();
    let expected_marker = fs::read(&marker).unwrap();
    fs::write(
        restore.join("user-memory.db"),
        b"tampered after publication",
    )
    .unwrap();

    let error = rehearse_complete_profile_backup(&backup, &restore).unwrap_err();

    assert!(
        matches!(&error, ProfileBackupError::CorruptBackup { message }
            if message.contains("checksum mismatch")),
        "unexpected error: {error}"
    );
    assert_eq!(fs::read(&marker).unwrap(), expected_marker);
    assert_eq!(
        fs::read(restore.join("user-memory.db")).unwrap(),
        b"tampered after publication"
    );
}

#[test]
fn marked_publication_recovery_rejects_partially_durable_restore() {
    let temp = TempDir::new().unwrap();
    let fixture = seed_released_profile(&temp);
    let backup = create_backup(&temp, &fixture);
    let restore = temp.path().join("rehearsed-profile");
    let marker = restore.join(".tracedecay-profile-rehearsal.json");
    set_rehearsal_publication_fault_for_test("after_rename_before_parent_sync");
    rehearse_complete_profile_backup(&backup, &restore).unwrap_err();
    let expected_marker = fs::read(&marker).unwrap();
    fs::remove_file(restore.join("user-sessions.db")).unwrap();

    let error = rehearse_complete_profile_backup(&backup, &restore).unwrap_err();

    assert!(
        matches!(&error, ProfileBackupError::CorruptBackup { message }
            if message.contains("user-sessions.db")),
        "unexpected error: {error}"
    );
    assert_eq!(fs::read(&marker).unwrap(), expected_marker);
    assert!(!restore.join("user-sessions.db").exists());
}

#[test]
fn marked_publication_recovery_rejects_tampered_rebound_manifest() {
    let temp = TempDir::new().unwrap();
    let fixture = seed_released_profile(&temp);
    let backup = create_backup(&temp, &fixture);
    let restore = temp.path().join("rehearsed-profile");
    let marker = restore.join(".tracedecay-profile-rehearsal.json");
    set_rehearsal_publication_fault_for_test("after_rename_before_parent_sync");
    rehearse_complete_profile_backup(&backup, &restore).unwrap_err();
    let expected_marker = fs::read(&marker).unwrap();
    let manifest_path = restore
        .join("projects")
        .join(fixture.project_id)
        .join(STORE_MANIFEST_FILENAME);
    let mut manifest = read_store_manifest(&manifest_path).unwrap();
    manifest.data_root = temp.path().join("tampered-store-root");
    write_store_manifest_to_path(&manifest_path, &manifest).unwrap();

    let error = rehearse_complete_profile_backup(&backup, &restore).unwrap_err();

    assert!(
        matches!(&error, ProfileBackupError::CorruptBackup { message }
            if message.contains("rebound backup manifest")),
        "unexpected error: {error}"
    );
    assert_eq!(fs::read(&marker).unwrap(), expected_marker);
    assert_eq!(
        read_store_manifest(&manifest_path).unwrap().data_root,
        temp.path().join("tampered-store-root")
    );
}

#[test]
fn released_copy_rehearsal_rejects_same_id_from_another_backup_root() {
    let original_temp = TempDir::new().unwrap();
    let original_fixture = seed_released_profile(&original_temp);
    let original_backup = create_backup(&original_temp, &original_fixture);
    let restore = original_temp.path().join("rehearsed-profile");
    let marker_path = restore.join(".tracedecay-profile-rehearsal.json");
    let expected_identity = read_bytes(&original_fixture.profile, "profile-identity.json");
    set_rehearsal_publication_fault_for_test("after_rename_before_parent_sync");

    rehearse_complete_profile_backup(&original_backup, &restore).unwrap_err();
    let expected_marker = fs::read(&marker_path).unwrap();

    let foreign_temp = TempDir::new().unwrap();
    let foreign_fixture = seed_released_profile(&foreign_temp);
    write_profile_identity(&foreign_fixture.profile, "brain.foreign", "profile.foreign");
    let foreign_backup = create_backup(&foreign_temp, &foreign_fixture);

    let error = rehearse_complete_profile_backup(&foreign_backup, &restore).unwrap_err();

    // The recovery invariant: an interrupted publication owned by a different
    // rehearsal is a typed conflict, never finished or cleared collaterally.
    assert!(
        matches!(&error, ProfileBackupError::PartialRestoreConflict { message }
            if message.contains("belongs to another restore attempt")),
        "unexpected error: {error}"
    );
    assert_eq!(fs::read(&marker_path).unwrap(), expected_marker);
    assert_eq!(
        read_bytes(&restore, "profile-identity.json"),
        expected_identity
    );
}
