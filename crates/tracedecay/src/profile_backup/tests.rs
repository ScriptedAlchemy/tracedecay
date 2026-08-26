use std::fs;
use std::path::{Path, PathBuf};

use super::{
    CompleteProfileBackupManifest, ProfileBackupError, REHEARSAL_MARKER_FILENAME,
    create_complete_profile_backup, rehearse_complete_profile_backup,
    set_rehearsal_publication_fault_for_test,
};

const FIXTURE_BRAIN_ID: &str = "brain.release-fixture";
const FIXTURE_PROFILE_ID: &str = "profile.release-fixture";

fn write_profile_identity(root: &Path) {
    let path = root.join("profile-identity.json");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "brain_id": FIXTURE_BRAIN_ID,
            "profile_id": FIXTURE_PROFILE_ID,
        }))
        .unwrap(),
    )
    .unwrap();
    restrict_file(&path);
}

#[cfg(unix)]
fn restrict_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[cfg(not(unix))]
fn restrict_file(_path: &Path) {}

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
    ] {
        let path = root.join(name);
        fs::write(&path, format!("released fixture: {name}")).unwrap();
    }
    write_profile_identity(root);
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

fn exclusive_lease(profile: &Path) -> tracedecay_runtime_core::lifecycle_lease::LifecycleLease {
    tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(profile, "backup test")
        .unwrap()
}

#[test]
fn complete_backup_rehearses_from_restored_isolated_copy() {
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("profile");
    let backups = temp.path().join("backups");
    let restore = temp.path().join("restored");
    fs::create_dir(&profile).unwrap();
    released_profile(&profile);
    let lease = exclusive_lease(&profile);

    let backup =
        create_complete_profile_backup(&profile, &backups, "backup.release", 100, &lease).unwrap();
    let manifest = rehearse_complete_profile_backup(&backup, &restore).unwrap();

    // Database sidecars are folded into snapshot artifacts, never inventoried.
    assert!(manifest.entries.iter().all(
        |entry| !entry.logical_path.ends_with("-wal") && !entry.logical_path.ends_with("-shm")
    ));
    assert!(
        manifest
            .entries
            .iter()
            .any(|entry| entry.logical_path == "projects/project.release.db" && entry.present)
    );
    assert_eq!(manifest.source_brain_id, FIXTURE_BRAIN_ID);
    assert_eq!(manifest.source_profile_id, FIXTURE_PROFILE_ID);
    assert_eq!(
        fs::read(restore.join("user-memory.db")).unwrap(),
        fs::read(profile.join("user-memory.db")).unwrap()
    );
    assert!(!restore.join("user-memory.db-wal").exists());
}

fn released_store_manifest(
    project_id: &str,
    project_root: &Path,
    data_root: &Path,
) -> tracedecay_runtime_core::storage::StoreManifest {
    tracedecay_runtime_core::storage::StoreManifest {
        schema_version: tracedecay_runtime_core::storage::STORE_MANIFEST_SCHEMA_VERSION,
        project_id: Some(project_id.to_owned()),
        store_kind: tracedecay_runtime_core::storage::StoreKind::CodeProject,
        storage_mode: tracedecay_runtime_core::storage::StorageMode::ProfileSharded,
        project_root: project_root.to_path_buf(),
        data_root: data_root.to_path_buf(),
        graph_db_relpath: "tracedecay.db".into(),
        sessions_db_relpath: "sessions.db".into(),
        branch_meta_relpath: "branch-meta.json".into(),
    }
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
    let source_manifest = released_store_manifest(project_id, &project, &source_store);
    tracedecay_runtime_core::storage::write_store_manifest_to_path(
        &source_store.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
        &source_manifest,
    )
    .unwrap();
    let expected_profile_identity = fs::read(profile.join("profile-identity.json")).unwrap();
    let expected_memory = fs::read(profile.join("user-memory.db")).unwrap();
    let expected_lcm = fs::read(profile.join("user-sessions.db")).unwrap();
    let expected_config = fs::read(profile.join("config.toml")).unwrap();
    let lease = exclusive_lease(&profile);

    let backup =
        create_complete_profile_backup(&profile, &backups, "backup.release", 100, &lease).unwrap();
    let manifest = rehearse_complete_profile_backup(&backup, &restore).unwrap();
    assert_eq!(manifest.projects.len(), 1);
    assert_eq!(manifest.projects[0].project_id, project_id);
    assert_eq!(manifest.projects[0].project_root, project);

    let restored_store = restore.join("projects").join(project_id);
    let restored_manifest = tracedecay_runtime_core::storage::read_store_manifest(
        &restored_store.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
    )
    .unwrap();
    assert_eq!(restored_manifest.project_id.as_deref(), Some(project_id));
    assert_eq!(restored_manifest.project_root, project);
    assert_eq!(
        restored_manifest.data_root,
        restored_store.canonicalize().unwrap()
    );
    assert_eq!(
        tracedecay_runtime_core::storage::read_store_manifest(
            &source_store.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME)
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
    let lease = exclusive_lease(&profile);
    let backup =
        create_complete_profile_backup(&profile, &backups, "backup.release", 100, &lease).unwrap();
    fs::write(backup.join("global.db"), b"corrupt").unwrap();

    let error =
        rehearse_complete_profile_backup(&backup, &temp.path().join("restored")).unwrap_err();
    assert!(
        matches!(&error, ProfileBackupError::CorruptBackup { message }
            if message.contains("checksum mismatch")),
        "unexpected error: {error}"
    );
}

#[test]
fn rehearsal_rejects_identity_tampered_backup_material() {
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("profile");
    let backups = temp.path().join("backups");
    let restore = temp.path().join("restored");
    fs::create_dir(&profile).unwrap();
    released_profile(&profile);
    let lease = exclusive_lease(&profile);
    let backup =
        create_complete_profile_backup(&profile, &backups, "backup.release", 100, &lease).unwrap();
    let tampered = backup.join("profile-identity.json");
    fs::write(
        &tampered,
        br#"{"schema_version":1,"brain_id":"brain.foreign","profile_id":"profile.foreign"}"#,
    )
    .unwrap();
    restrict_file(&tampered);

    let error = rehearse_complete_profile_backup(&backup, &restore).unwrap_err();

    assert!(
        matches!(&error, ProfileBackupError::CorruptBackup { message }
            if message.contains("checksum mismatch")),
        "unexpected error: {error}"
    );
    assert!(!restore.exists());
}

#[test]
fn rehearsal_rejects_an_older_manifest_schema_as_reset_required() {
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("profile");
    let backups = temp.path().join("backups");
    fs::create_dir(&profile).unwrap();
    released_profile(&profile);
    let lease = exclusive_lease(&profile);
    let backup =
        create_complete_profile_backup(&profile, &backups, "backup.release", 100, &lease).unwrap();
    let manifest_path = backup.join("backup-manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["schema_version"] = serde_json::Value::from(1);
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let error =
        rehearse_complete_profile_backup(&backup, &temp.path().join("restored")).unwrap_err();
    assert!(
        matches!(error, ProfileBackupError::ResetRequired { .. }),
        "unexpected error: {error}"
    );
}

#[test]
fn backup_refuses_destination_inside_live_profile() {
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("profile");
    fs::create_dir(&profile).unwrap();
    released_profile(&profile);
    let lease = exclusive_lease(&profile);

    let error = create_complete_profile_backup(
        &profile,
        &profile.join("backups"),
        "backup.release",
        100,
        &lease,
    )
    .unwrap_err();
    assert!(
        matches!(&error, ProfileBackupError::InvalidRequest { message }
            if message.contains("outside the source profile")),
        "unexpected error: {error}"
    );
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
    tracedecay_runtime_core::storage::write_store_manifest_to_path(
        &source_store.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
        &released_store_manifest(project_id, &project, &source_store),
    )
    .unwrap();
    let lease = exclusive_lease(&profile);
    let backup =
        create_complete_profile_backup(&profile, &backups, "backup.release", 100, &lease).unwrap();
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
            matches!(&error, ProfileBackupError::Unavailable { message }
                if message.contains("injected rehearsal publication fault")),
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
        tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME
    );
    fs::remove_file(backup.join(&manifest_entry)).unwrap();
    let manifest_path = backup.join("backup-manifest.json");
    let mut manifest: CompleteProfileBackupManifest =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest
        .entries
        .retain(|entry| entry.logical_path != manifest_entry);
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
