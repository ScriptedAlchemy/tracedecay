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
    ] {
        let path = root.join(name);
        fs::write(&path, format!("released fixture: {name}")).unwrap();
    }
    fs::write(
        root.join("profile-identity.json"),
        br#"{"schema_version":1,"brain_id":"brain.release","profile_id":"profile.release"}"#,
    )
    .unwrap();
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
    let lease = tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(
        &profile,
        "backup test",
    )
    .unwrap();

    let backup =
        create_complete_profile_backup(&profile, &backups, "backup.release", 100, &lease).unwrap();
    let manifest = rehearse_complete_profile_backup(&backup, &restore).unwrap();

    assert!(
        manifest
            .entries
            .iter()
            .all(|entry| entry.logical_path != "user-sessions.db-wal")
    );
    assert!(
        manifest
            .entries
            .iter()
            .any(|entry| entry.logical_path == "projects/project.release.db" && entry.present)
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
    let source_manifest = tracedecay_runtime_core::storage::StoreManifest {
        schema_version: tracedecay_runtime_core::storage::STORE_MANIFEST_SCHEMA_VERSION,
        project_id: Some(project_id.to_owned()),
        store_kind: tracedecay_runtime_core::storage::StoreKind::CodeProject,
        storage_mode: tracedecay_runtime_core::storage::StorageMode::ProfileSharded,
        project_root: project.clone(),
        data_root: source_store.clone(),
        graph_db_relpath: "tracedecay.db".into(),
        sessions_db_relpath: "sessions.db".into(),
        branch_meta_relpath: "branch-meta.json".into(),
    };
    tracedecay_runtime_core::storage::write_store_manifest_to_path(
        &source_store.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
        &source_manifest,
    )
    .unwrap();
    let expected_profile_identity = fs::read(profile.join("profile-identity.json")).unwrap();
    let expected_memory = fs::read(profile.join("user-memory.db")).unwrap();
    let expected_lcm = fs::read(profile.join("user-sessions.db")).unwrap();
    let expected_config = fs::read(profile.join("config.toml")).unwrap();
    let lease = tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(
        &profile,
        "backup test",
    )
    .unwrap();

    let backup =
        create_complete_profile_backup(&profile, &backups, "backup.release", 100, &lease).unwrap();
    rehearse_complete_profile_backup(&backup, &restore).unwrap();

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
    let lease = tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(
        &profile,
        "backup test",
    )
    .unwrap();
    let backup =
        create_complete_profile_backup(&profile, &backups, "backup.release", 100, &lease).unwrap();
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
    let lease = tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(
        &profile,
        "backup test",
    )
    .unwrap();

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
    tracedecay_runtime_core::storage::write_store_manifest_to_path(
        &source_store.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
        &tracedecay_runtime_core::storage::StoreManifest {
            schema_version: tracedecay_runtime_core::storage::STORE_MANIFEST_SCHEMA_VERSION,
            project_id: Some(project_id.to_owned()),
            store_kind: tracedecay_runtime_core::storage::StoreKind::CodeProject,
            storage_mode: tracedecay_runtime_core::storage::StorageMode::ProfileSharded,
            project_root: project,
            data_root: source_store.clone(),
            graph_db_relpath: "tracedecay.db".into(),
            sessions_db_relpath: "sessions.db".into(),
            branch_meta_relpath: "branch-meta.json".into(),
        },
    )
    .unwrap();
    let lease = tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(
        &profile,
        "backup test",
    )
    .unwrap();
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
        error.contains("missing required store_manifest.json"),
        "unexpected error: {error}"
    );
    assert!(!restore.exists());
}
