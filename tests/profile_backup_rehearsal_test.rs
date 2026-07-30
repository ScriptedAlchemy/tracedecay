use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tracedecay::migrate::profile_backup::{
    create_complete_profile_backup, rehearse_complete_profile_backup,
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
        "profile-identity.json",
    ] {
        fs::write(profile.join(name), format!("released fixture: {name}")).unwrap();
    }
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
}

#[test]
fn released_copy_rehearsal_recovers_owned_interrupted_staging() {
    let temp = TempDir::new().unwrap();
    let fixture = seed_released_profile(&temp);
    let backup = create_backup(&temp, &fixture);
    let restore = temp.path().join("rehearsed-profile");
    let staging = temp.path().join(".rehearsed-profile.tracedecay-rehearsal");
    fs::create_dir(&staging).unwrap();
    fs::write(staging.join("partial"), b"interrupted copy").unwrap();
    fs::write(
        staging.join(".tracedecay-profile-rehearsal.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "backup_id": "backup.release",
            "restore_root": restore.clone(),
        }))
        .unwrap(),
    )
    .unwrap();

    rehearse_complete_profile_backup(&backup, &restore).unwrap();

    assert!(restore.join("profile-identity.json").is_file());
    assert!(!staging.exists());
    assert!(!restore.join("partial").exists());
}
