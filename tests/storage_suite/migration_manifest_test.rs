use std::fs;
use std::path::{Path, PathBuf};

use crate::common::sample_node;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use tempfile::TempDir;
use tracedecay::branch_meta::BranchMeta;
use tracedecay::lifecycle_lease::acquire_exclusive_for_profile;
use tracedecay::migrate::inventory::{
    MigrationInventory, RegistryStatus, StoreArtifact, StoreBrand, StoreInventory, StoreRole,
    StoreStatus,
};
use tracedecay::migrate::manifest::{
    ArtifactState, MigrationArtifact, MigrationManifest, MigrationPlanOptions, MigrationProtocol,
    MigrationRollbackState, apply_migration_manifest, assess_migration_rollback_state,
    build_plan_manifest, cleanup_migration_sources, export_profile_store,
    export_profile_store_with_lease, finalize_migration_apply, load_manifest,
    rollback_migration_manifest, save_manifest, verify_migration_manifest,
};
use tracedecay::storage::{
    EnrollmentMarker, STORE_MANIFEST_FILENAME, STORE_MANIFEST_SCHEMA_VERSION, StorageMode,
    StoreKind, StoreManifest, read_enrollment_marker, write_enrollment_marker,
};

fn empty_inventory() -> MigrationInventory {
    MigrationInventory {
        stores: Vec::new(),
        skipped: Vec::new(),
        global_db: None,
    }
}

fn manifest_for(protocol: MigrationProtocol, migration_id: &str) -> MigrationManifest {
    MigrationManifest::new(
        migration_id,
        "0.0.2",
        1_800_000_000,
        format!("confirm-{migration_id}"),
        protocol,
        empty_inventory(),
    )
}

fn canonical_temp_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn sqlite_sidecar(database: &Path, suffix: &str) -> PathBuf {
    let mut path = database.as_os_str().to_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

fn write_valid_branch_meta(path: &Path) {
    fs::write(
        path,
        serde_json::to_vec_pretty(&BranchMeta::new("main")).unwrap(),
    )
    .unwrap();
}

#[cfg(unix)]
#[test]
fn save_manifest_rejects_symlinked_parent_components() {
    let dir = TempDir::new().unwrap();
    let outside = dir.path().join("outside");
    let link = dir.path().join("profile-link");
    fs::create_dir_all(&outside).unwrap();
    symlink(&outside, &link).unwrap();
    let manifest_path = link.join("migration-manifest.json");
    let manifest = manifest_for(
        MigrationProtocol::for_manifest(&manifest_path, "mig_123"),
        "mig_123",
    );

    let err = save_manifest(&manifest).unwrap_err();

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(!outside.join("migration-manifest.json").exists());
}

#[test]
fn plan_manifest_maps_inventory_artifacts_into_profile_shard_targets() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let data_dir = project.join(".tracedecay");
    let graph_db = data_dir.join("tracedecay.db");
    let profile_root = dir.path().join("profile");
    let inventory = MigrationInventory {
        stores: vec![StoreInventory {
            project_root: project.clone(),
            data_dir: data_dir.clone(),
            db_path: graph_db.clone(),
            brand: StoreBrand::TraceDecay,
            role: StoreRole::CodeProjectStore,
            registry_status: RegistryStatus::Unregistered,
            size_bytes: 128,
            statuses: vec![StoreStatus::Ok],
            artifacts: vec![
                StoreArtifact {
                    kind: "graph_db".to_string(),
                    path: graph_db.clone(),
                    size_bytes: 128,
                },
                StoreArtifact {
                    kind: "sessions_db".to_string(),
                    path: data_dir.join("sessions.db"),
                    size_bytes: 64,
                },
                StoreArtifact {
                    kind: "sync_lock".to_string(),
                    path: data_dir.join("sync.lock"),
                    size_bytes: 0,
                },
            ],
        }],
        skipped: Vec::new(),
        global_db: None,
    };

    let manifest = build_plan_manifest(
        inventory,
        MigrationPlanOptions {
            manifest_path: dir.path().join("manifest.json"),
            migration_id: "mig_123".to_string(),
            tracedecay_version: "0.0.2".to_string(),
            created_at_unix: 1_800_000_000,
            confirmation_token: "confirm-mig_123".to_string(),
            target_profile_root: profile_root.clone(),
            project_id: "proj_123".to_string(),
        },
    )
    .unwrap();

    assert_eq!(manifest.artifacts.len(), 2);
    assert_eq!(manifest.backup_artifacts.len(), 2);
    assert!(
        manifest
            .artifacts
            .iter()
            .all(|artifact| artifact.kind != "sync_lock")
    );
    assert_eq!(manifest.artifacts[0].source_path, graph_db);
    assert_eq!(
        manifest.artifacts[0].target_path.as_deref(),
        Some(
            profile_root
                .join("projects/proj_123/tracedecay.db")
                .as_path()
        )
    );
    assert_eq!(
        manifest.artifacts[1].target_path.as_deref(),
        Some(profile_root.join("projects/proj_123/sessions.db").as_path())
    );
    assert!(
        manifest
            .artifacts
            .iter()
            .all(|artifact| artifact.state == ArtifactState::Planned)
    );
}

#[test]
fn plan_manifest_rejects_artifact_outside_store_data_dir() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let data_dir = project.join(".tracedecay");
    let graph_db = data_dir.join("tracedecay.db");
    let outside_db = dir.path().join("outside.db");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(&graph_db, b"graph").unwrap();
    fs::write(&outside_db, b"outside").unwrap();
    let inventory = MigrationInventory {
        stores: vec![StoreInventory {
            project_root: project,
            data_dir,
            db_path: graph_db,
            brand: StoreBrand::TraceDecay,
            role: StoreRole::CodeProjectStore,
            registry_status: RegistryStatus::Unregistered,
            size_bytes: 128,
            statuses: vec![StoreStatus::Ok],
            artifacts: vec![StoreArtifact {
                kind: "graph_db".to_string(),
                path: outside_db,
                size_bytes: 7,
            }],
        }],
        skipped: Vec::new(),
        global_db: None,
    };

    let err = build_plan_manifest(
        inventory,
        MigrationPlanOptions {
            manifest_path: dir.path().join("manifest.json"),
            migration_id: "mig_123".to_string(),
            tracedecay_version: "0.0.2".to_string(),
            created_at_unix: 1_800_000_000,
            confirmation_token: "confirm-mig_123".to_string(),
            target_profile_root: dir.path().join("profile"),
            project_id: "proj_123".to_string(),
        },
    )
    .unwrap_err();

    assert!(
        err.contains("outside store data_dir"),
        "unexpected error: {err}"
    );
}

#[test]
fn plan_manifest_rejects_unsafe_project_id() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let data_dir = project.join(".tracedecay");
    let graph_db = data_dir.join("tracedecay.db");
    let inventory = MigrationInventory {
        stores: vec![StoreInventory {
            project_root: project,
            data_dir,
            db_path: graph_db.clone(),
            brand: StoreBrand::TraceDecay,
            role: StoreRole::CodeProjectStore,
            registry_status: RegistryStatus::Unregistered,
            size_bytes: 128,
            statuses: vec![StoreStatus::Ok],
            artifacts: vec![StoreArtifact {
                kind: "graph_db".to_string(),
                path: graph_db,
                size_bytes: 128,
            }],
        }],
        skipped: Vec::new(),
        global_db: None,
    };

    let err = build_plan_manifest(
        inventory,
        MigrationPlanOptions {
            manifest_path: dir.path().join("manifest.json"),
            migration_id: "mig_123".to_string(),
            tracedecay_version: "0.0.2".to_string(),
            created_at_unix: 1_800_000_000,
            confirmation_token: "confirm-mig_123".to_string(),
            target_profile_root: dir.path().join("profile"),
            project_id: "../outside".to_string(),
        },
    )
    .unwrap_err();

    assert!(err.contains("invalid project_id"), "{err}");
}

#[test]
fn verify_manifest_validates_profile_store_manifest_registry_records() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let profile_root = dir.path().join("profile");
    let data_root = profile_root.join("projects/proj_123");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&data_root).unwrap();
    fs::write(data_root.join("tracedecay.db"), b"graph").unwrap();
    fs::write(data_root.join("sessions.db"), b"sessions").unwrap();
    write_valid_branch_meta(&data_root.join("branch-meta.json"));
    let store_manifest = StoreManifest {
        schema_version: STORE_MANIFEST_SCHEMA_VERSION,
        project_id: Some("proj_123".to_string()),
        store_kind: StoreKind::CodeProject,
        storage_mode: StorageMode::ProfileSharded,
        project_root: project,
        data_root: data_root.clone(),
        graph_db_relpath: "tracedecay.db".into(),
        sessions_db_relpath: "sessions.db".into(),
        branch_meta_relpath: "branch-meta.json".into(),
    };
    let store_manifest_path = data_root.join(STORE_MANIFEST_FILENAME);
    fs::write(
        &store_manifest_path,
        serde_json::to_string_pretty(&store_manifest).unwrap(),
    )
    .unwrap();
    let protocol = MigrationProtocol::for_manifest(dir.path().join("manifest.json"), "mig_123");
    let mut manifest = MigrationManifest::new(
        "mig_123",
        "0.0.2",
        1_800_000_000,
        "confirm-mig_123",
        protocol,
        MigrationInventory {
            stores: Vec::new(),
            skipped: Vec::new(),
            global_db: None,
        },
    );
    manifest.artifacts.push(MigrationArtifact::new(
        "graph_db",
        PathBuf::from("/source/tracedecay.db"),
        Some(data_root.join("tracedecay.db")),
    ));
    manifest.artifacts.push(MigrationArtifact::new(
        "store_manifest",
        PathBuf::from("/source/store_manifest.json"),
        Some(store_manifest_path),
    ));

    let report = verify_migration_manifest(&manifest);

    assert_eq!(report.artifact_count, 2);
    assert_eq!(report.missing_targets, 0);
    assert_eq!(report.store_manifest_count, 1);
    assert_eq!(report.registry_plan_count, 1);
    assert!(!report.apply_supported);
    assert!(report.issues.is_empty(), "{:?}", report.issues);
}

#[tokio::test]
async fn verify_manifest_rejects_sqlite_target_without_verified_snapshot() {
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let project = root.join("repo");
    let data_dir = project.join(".tracedecay");
    let source_db = data_dir.join("tracedecay.db");
    let profile_root = root.join("profile");
    let data_root = profile_root.join("projects/proj_123");
    let target_db = data_root.join("tracedecay.db");
    fs::create_dir_all(&data_dir).unwrap();
    fs::create_dir_all(&data_root).unwrap();

    let (source, _) = crate::common::initialize_test_database(&source_db)
        .await
        .unwrap();
    source
        .insert_node(&sample_node("node-1", "process_data", "src/lib.rs"))
        .await
        .unwrap();
    assert_eq!(source.get_all_nodes().await.unwrap().len(), 1);
    source.checkpoint().await.unwrap();
    source.close();

    let (target, _) = crate::common::initialize_test_database(&target_db)
        .await
        .unwrap();
    target
        .insert_node(&sample_node("node-extra", "deleted_later", "src/lib.rs"))
        .await
        .unwrap();
    target
        .insert_node(&sample_node("node-1", "process_data", "src/lib.rs"))
        .await
        .unwrap();
    assert_eq!(target.get_all_nodes().await.unwrap().len(), 2);
    target.checkpoint().await.unwrap();
    target.close();

    write_valid_branch_meta(&data_root.join("branch-meta.json"));
    fs::write(data_root.join("sessions.db"), b"sessions").unwrap();
    let store_manifest = StoreManifest {
        schema_version: STORE_MANIFEST_SCHEMA_VERSION,
        project_id: Some("proj_123".to_string()),
        store_kind: StoreKind::CodeProject,
        storage_mode: StorageMode::ProfileSharded,
        project_root: project.clone(),
        data_root: data_root.clone(),
        graph_db_relpath: "tracedecay.db".into(),
        sessions_db_relpath: "sessions.db".into(),
        branch_meta_relpath: "branch-meta.json".into(),
    };
    let store_manifest_path = data_root.join(STORE_MANIFEST_FILENAME);
    fs::write(
        &store_manifest_path,
        serde_json::to_string_pretty(&store_manifest).unwrap(),
    )
    .unwrap();
    fs::write(
        data_dir.join(STORE_MANIFEST_FILENAME),
        serde_json::to_string_pretty(&store_manifest).unwrap(),
    )
    .unwrap();
    write_enrollment_marker(
        &project,
        &EnrollmentMarker {
            project_id: "proj_123".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let protocol = MigrationProtocol::for_manifest(root.join("manifest.json"), "mig_123");
    let mut manifest = MigrationManifest::new(
        "mig_123",
        "0.0.2",
        1_800_000_000,
        "confirm-mig_123",
        protocol,
        MigrationInventory {
            stores: Vec::new(),
            skipped: Vec::new(),
            global_db: None,
        },
    );
    manifest.source.project_root = Some(project);
    manifest.source.data_dir = Some(data_dir.clone());
    manifest.destination.profile_root = Some(profile_root);
    manifest.destination.project_id = Some("proj_123".to_string());
    manifest.artifacts.push(MigrationArtifact {
        kind: "graph_db".to_string(),
        source_path: source_db,
        target_path: Some(target_db),
        state: ArtifactState::Applied,
    });
    manifest.artifacts.push(MigrationArtifact {
        kind: "store_manifest".to_string(),
        source_path: data_dir.join("store_manifest.json"),
        target_path: Some(store_manifest_path),
        state: ArtifactState::Applied,
    });

    let report = verify_migration_manifest(&manifest);

    assert!(!report.apply_supported);
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.contains("verified SQLite migration backup is missing")),
        "{:?}",
        report.issues
    );
}

#[test]
fn verify_manifest_rejects_corrupt_verified_sqlite_backup() {
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let data_dir = root.join("repo/.tracedecay");
    let source_db = data_dir.join("tracedecay.db");
    let profile_root = root.join("profile");
    let backup_db = profile_root.join("migration-backups/mig_corrupt/tracedecay.db");
    fs::create_dir_all(&data_dir).unwrap();
    fs::create_dir_all(backup_db.parent().unwrap()).unwrap();
    let connection = rusqlite::Connection::open(&source_db).unwrap();
    connection
        .execute_batch("CREATE TABLE nodes (id INTEGER PRIMARY KEY, name TEXT);")
        .unwrap();
    drop(connection);
    fs::copy(&source_db, &backup_db).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&backup_db)
        .unwrap()
        .set_len(100)
        .unwrap();
    let protocol = MigrationProtocol::for_manifest(root.join("manifest.json"), "mig_corrupt");
    let mut manifest = manifest_for(protocol, "mig_corrupt");
    manifest.source.data_dir = Some(data_dir);
    manifest.destination.profile_root = Some(profile_root);
    manifest.backup_artifacts.push(MigrationArtifact {
        kind: "graph_db".to_string(),
        source_path: source_db,
        target_path: Some(backup_db),
        state: ArtifactState::Verified,
    });

    let report = verify_migration_manifest(&manifest);

    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.contains("quick_check")),
        "{:?}",
        report.issues
    );
}

#[tokio::test]
async fn apply_resume_refuses_corrupt_existing_backup_before_publishing_target() {
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let project = root.join("repo");
    let data_dir = project.join(".tracedecay");
    let source_db = data_dir.join("tracedecay.db");
    let profile_root = root.join("profile");
    let manifest_path = root.join("manifest.json");
    fs::create_dir_all(&data_dir).unwrap();
    let source = rusqlite::Connection::open(&source_db).unwrap();
    source
        .execute_batch(
            "CREATE TABLE migration_probe (value TEXT NOT NULL);
             INSERT INTO migration_probe VALUES ('source-preserved');",
        )
        .unwrap();
    drop(source);

    let mut manifest = build_plan_manifest(
        MigrationInventory {
            stores: vec![StoreInventory {
                project_root: project.clone(),
                data_dir: data_dir.clone(),
                db_path: source_db.clone(),
                brand: StoreBrand::TraceDecay,
                role: StoreRole::CodeProjectStore,
                registry_status: RegistryStatus::Unregistered,
                size_bytes: fs::metadata(&source_db).unwrap().len(),
                statuses: vec![StoreStatus::Ok],
                artifacts: vec![StoreArtifact {
                    kind: "graph_db".to_string(),
                    path: source_db.clone(),
                    size_bytes: fs::metadata(&source_db).unwrap().len(),
                }],
            }],
            skipped: Vec::new(),
            global_db: None,
        },
        MigrationPlanOptions {
            manifest_path: manifest_path.clone(),
            migration_id: "mig_corrupt_resume".to_string(),
            tracedecay_version: "0.0.2".to_string(),
            created_at_unix: 1_800_000_000,
            confirmation_token: "confirm-mig_corrupt_resume".to_string(),
            target_profile_root: profile_root.clone(),
            project_id: "proj_123".to_string(),
        },
    )
    .unwrap();
    let backup = manifest
        .backup_artifacts
        .iter_mut()
        .find(|artifact| artifact.kind == "graph_db")
        .unwrap();
    let backup_path = backup.target_path.clone().unwrap();
    fs::create_dir_all(backup_path.parent().unwrap()).unwrap();
    fs::write(&backup_path, b"corrupt published backup").unwrap();
    backup.state = ArtifactState::Locked;
    save_manifest(&manifest).unwrap();
    drop(manifest);
    let mut resumed = load_manifest(&manifest_path).unwrap();

    let error = apply_migration_manifest(&mut resumed).await.unwrap_err();

    assert!(
        error.to_string().contains("file is not a database")
            || error
                .to_string()
                .contains("database disk image is malformed")
            || error.to_string().contains("failed quick_check"),
        "unexpected error: {error}"
    );
    assert_eq!(fs::read(&backup_path).unwrap(), b"corrupt published backup");
    assert_eq!(
        resumed
            .backup_artifacts
            .iter()
            .find(|artifact| artifact.kind == "graph_db")
            .unwrap()
            .state,
        ArtifactState::Locked
    );
    assert!(
        !profile_root
            .join("projects/proj_123/tracedecay.db")
            .exists()
    );
    assert!(read_enrollment_marker(&project).unwrap().is_none());
    assert_eq!(
        rusqlite::Connection::open_with_flags(
            &source_db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
        .query_row("SELECT value FROM migration_probe", (), |row| {
            row.get::<_, String>(0)
        })
        .unwrap(),
        "source-preserved"
    );
}

#[tokio::test]
async fn apply_migration_manifest_stops_at_verified_before_cutover() {
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let manifest_path = root.join("manifest.json");
    let project = root.join("repo");
    let data_dir = project.join(".tracedecay");
    let graph_db = data_dir.join("tracedecay.db");
    let sessions_db = data_dir.join("sessions.db");
    let branch_meta = data_dir.join("branch-meta.json");
    let profile_root = root.join("profile");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(&graph_db, b"graph").unwrap();
    fs::write(&sessions_db, b"sessions").unwrap();
    write_valid_branch_meta(&branch_meta);
    let mut manifest = build_plan_manifest(
        MigrationInventory {
            stores: vec![StoreInventory {
                project_root: project.clone(),
                data_dir: data_dir.clone(),
                db_path: graph_db.clone(),
                brand: StoreBrand::TraceDecay,
                role: StoreRole::CodeProjectStore,
                registry_status: RegistryStatus::Unregistered,
                size_bytes: 128,
                statuses: vec![StoreStatus::Ok],
                artifacts: vec![
                    StoreArtifact {
                        kind: "graph_db".to_string(),
                        path: graph_db.clone(),
                        size_bytes: 5,
                    },
                    StoreArtifact {
                        kind: "sessions_db".to_string(),
                        path: sessions_db.clone(),
                        size_bytes: 8,
                    },
                    StoreArtifact {
                        kind: "branch_meta".to_string(),
                        path: branch_meta.clone(),
                        size_bytes: fs::metadata(&branch_meta).unwrap().len(),
                    },
                ],
            }],
            skipped: Vec::new(),
            global_db: None,
        },
        MigrationPlanOptions {
            manifest_path: manifest_path.clone(),
            migration_id: "mig_123".to_string(),
            tracedecay_version: "0.0.2".to_string(),
            created_at_unix: 1_800_000_000,
            confirmation_token: "confirm-mig_123".to_string(),
            target_profile_root: profile_root.clone(),
            project_id: "proj_123".to_string(),
        },
    )
    .unwrap();
    save_manifest(&manifest).unwrap();

    apply_migration_manifest(&mut manifest).await.unwrap();

    assert!(
        manifest
            .artifacts
            .iter()
            .all(|artifact| artifact.state == ArtifactState::Verified)
    );
    assert!(read_enrollment_marker(&project).unwrap().is_none());
    let verify = verify_migration_manifest(&manifest);
    assert!(verify.cutover_ready, "{:?}", verify.issues);
    assert!(!verify.apply_supported, "{:?}", verify.issues);
}

#[tokio::test]
async fn finalize_migration_apply_marks_cutover_complete() {
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let manifest_path = root.join("manifest.json");
    let project = root.join("repo");
    let data_dir = project.join(".tracedecay");
    let graph_db = data_dir.join("tracedecay.db");
    let profile_root = root.join("profile");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(&graph_db, b"graph").unwrap();
    write_valid_branch_meta(&data_dir.join("branch-meta.json"));
    let mut manifest = build_plan_manifest(
        MigrationInventory {
            stores: vec![StoreInventory {
                project_root: project.clone(),
                data_dir: data_dir.clone(),
                db_path: graph_db.clone(),
                brand: StoreBrand::TraceDecay,
                role: StoreRole::CodeProjectStore,
                registry_status: RegistryStatus::Unregistered,
                size_bytes: 128,
                statuses: vec![StoreStatus::Ok],
                artifacts: vec![StoreArtifact {
                    kind: "graph_db".to_string(),
                    path: graph_db.clone(),
                    size_bytes: 5,
                }],
            }],
            skipped: Vec::new(),
            global_db: None,
        },
        MigrationPlanOptions {
            manifest_path,
            migration_id: "mig_123".to_string(),
            tracedecay_version: "0.0.2".to_string(),
            created_at_unix: 1_800_000_000,
            confirmation_token: "confirm-mig_123".to_string(),
            target_profile_root: profile_root,
            project_id: "proj_123".to_string(),
        },
    )
    .unwrap();
    apply_migration_manifest(&mut manifest).await.unwrap();
    tracedecay::storage::write_enrollment_marker(
        &project,
        &tracedecay::storage::EnrollmentMarker {
            project_id: "proj_123".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();

    finalize_migration_apply(&mut manifest).unwrap();

    assert!(
        manifest
            .artifacts
            .iter()
            .all(|artifact| artifact.state == ArtifactState::Applied)
    );
    assert!(verify_migration_manifest(&manifest).apply_supported);
}

#[tokio::test]
async fn apply_migration_manifest_rejects_target_parent_escape() {
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let manifest_path = root.join("manifest.json");
    let project = root.join("repo");
    let data_dir = project.join(".tracedecay");
    let graph_db = data_dir.join("tracedecay.db");
    let profile_root = root.join("profile");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(&graph_db, b"graph").unwrap();
    let mut manifest = build_plan_manifest(
        MigrationInventory {
            stores: vec![StoreInventory {
                project_root: project,
                data_dir: data_dir.clone(),
                db_path: graph_db.clone(),
                brand: StoreBrand::TraceDecay,
                role: StoreRole::CodeProjectStore,
                registry_status: RegistryStatus::Unregistered,
                size_bytes: 5,
                statuses: vec![StoreStatus::Ok],
                artifacts: vec![StoreArtifact {
                    kind: "graph_db".to_string(),
                    path: graph_db,
                    size_bytes: 5,
                }],
            }],
            skipped: Vec::new(),
            global_db: None,
        },
        MigrationPlanOptions {
            manifest_path,
            migration_id: "mig_123".to_string(),
            tracedecay_version: "0.0.2".to_string(),
            created_at_unix: 1_800_000_000,
            confirmation_token: "confirm-mig_123".to_string(),
            target_profile_root: profile_root.clone(),
            project_id: "proj_123".to_string(),
        },
    )
    .unwrap();
    let escaped_target = profile_root.join("projects/proj_123/../../escaped.db");
    manifest.artifacts[0].target_path = Some(escaped_target.clone());
    save_manifest(&manifest).unwrap();

    let err = apply_migration_manifest(&mut manifest).await.unwrap_err();

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(
        err.to_string().contains("outside profile shard") || err.to_string().contains("traversal"),
        "unexpected error: {err}"
    );
    assert!(!root.join("profile/escaped.db").exists());
    assert!(!escaped_target.exists());
}

#[test]
fn cleanup_migration_sources_rejects_source_parent_escape_without_deleting_outside_file() {
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let manifest_path = root.join("manifest.json");
    let protocol = MigrationProtocol::for_manifest(&manifest_path, "mig_123");
    let project = root.join("repo");
    let data_dir = project.join(".tracedecay");
    let profile_root = root.join("profile");
    let data_root = profile_root.join("projects/proj_123");
    let escaped_source = data_dir.join("../outside.db");
    let outside_file = project.join("outside.db");
    let target = data_root.join("tracedecay.db");
    fs::create_dir_all(&data_dir).unwrap();
    fs::create_dir_all(&data_root).unwrap();
    fs::write(&outside_file, b"outside").unwrap();
    fs::write(&target, b"outside").unwrap();
    tracedecay::storage::write_enrollment_marker(
        &project,
        &tracedecay::storage::EnrollmentMarker {
            project_id: "proj_123".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    fs::write(
        data_root.join(STORE_MANIFEST_FILENAME),
        serde_json::to_string_pretty(&StoreManifest {
            schema_version: STORE_MANIFEST_SCHEMA_VERSION,
            project_id: Some("proj_123".to_string()),
            store_kind: StoreKind::CodeProject,
            storage_mode: StorageMode::ProfileSharded,
            project_root: project.clone(),
            data_root: data_root.clone(),
            graph_db_relpath: "tracedecay.db".into(),
            sessions_db_relpath: "sessions.db".into(),
            branch_meta_relpath: "branch-meta.json".into(),
        })
        .unwrap(),
    )
    .unwrap();
    let mut manifest = MigrationManifest::new(
        "mig_123",
        "0.0.2",
        1_800_000_000,
        "confirm-mig_123",
        protocol,
        MigrationInventory {
            stores: vec![StoreInventory {
                project_root: project.clone(),
                data_dir: data_dir.clone(),
                db_path: outside_file.clone(),
                brand: StoreBrand::TraceDecay,
                role: StoreRole::CodeProjectStore,
                registry_status: RegistryStatus::Unregistered,
                size_bytes: 7,
                statuses: vec![StoreStatus::Ok],
                artifacts: Vec::new(),
            }],
            skipped: Vec::new(),
            global_db: None,
        },
    );
    manifest.source.project_root = Some(project);
    manifest.source.data_dir = Some(data_dir);
    manifest.destination.profile_root = Some(profile_root);
    manifest.destination.project_id = Some("proj_123".to_string());
    manifest.artifacts.push(MigrationArtifact {
        kind: "graph_db".to_string(),
        source_path: escaped_source,
        target_path: Some(target),
        state: ArtifactState::Applied,
    });
    manifest.artifacts.push(MigrationArtifact {
        kind: "store_manifest".to_string(),
        source_path: data_root.join(STORE_MANIFEST_FILENAME),
        target_path: Some(data_root.join(STORE_MANIFEST_FILENAME)),
        state: ArtifactState::Applied,
    });
    save_manifest(&manifest).unwrap();

    let err = cleanup_migration_sources(&manifest).unwrap_err();

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(
        err.to_string().contains("outside source store") || err.to_string().contains("traversal"),
        "unexpected error: {err}"
    );
    assert_eq!(fs::read(&outside_file).unwrap(), b"outside");
}

#[test]
fn export_profile_store_requires_exclusive_profile_lifecycle_cutover() {
    let dir = TempDir::new().unwrap();
    let profile_root = canonical_temp_path(&dir.path().join("profile"));
    let project_id = "proj_123";
    let data_root = profile_root.join("projects").join(project_id);
    let project_root = dir.path().join("repo");
    let target = dir.path().join("export");
    fs::create_dir_all(&data_root).unwrap();
    fs::create_dir_all(&project_root).unwrap();
    fs::write(data_root.join("tracedecay.db"), b"database").unwrap();
    fs::write(
        data_root.join(STORE_MANIFEST_FILENAME),
        serde_json::to_vec_pretty(&StoreManifest {
            schema_version: STORE_MANIFEST_SCHEMA_VERSION,
            project_id: Some(project_id.to_string()),
            store_kind: StoreKind::CodeProject,
            storage_mode: StorageMode::ProfileSharded,
            project_root,
            data_root: data_root.clone(),
            graph_db_relpath: "tracedecay.db".into(),
            sessions_db_relpath: "sessions.db".into(),
            branch_meta_relpath: "branch-meta.json".into(),
        })
        .unwrap(),
    )
    .unwrap();
    let _lease = acquire_exclusive_for_profile(&profile_root, "test export owner").unwrap();

    let error = export_profile_store(&profile_root, project_id, &target).unwrap_err();

    assert!(error.to_string().contains("test export owner"));
    assert!(!target.exists());
}

#[test]
fn export_profile_store_reuses_caller_exclusive_profile_lifecycle() {
    let dir = TempDir::new().unwrap();
    let profile_root = canonical_temp_path(&dir.path().join("profile"));
    let project_id = "proj_123";
    let data_root = profile_root.join("projects").join(project_id);
    let project_root = dir.path().join("repo");
    let target = dir.path().join("export");
    fs::create_dir_all(&data_root).unwrap();
    fs::create_dir_all(&project_root).unwrap();
    fs::write(data_root.join("tracedecay.db"), b"database").unwrap();
    fs::write(
        data_root.join(STORE_MANIFEST_FILENAME),
        serde_json::to_vec_pretty(&StoreManifest {
            schema_version: STORE_MANIFEST_SCHEMA_VERSION,
            project_id: Some(project_id.to_string()),
            store_kind: StoreKind::CodeProject,
            storage_mode: StorageMode::ProfileSharded,
            project_root,
            data_root: data_root.clone(),
            graph_db_relpath: "tracedecay.db".into(),
            sessions_db_relpath: "sessions.db".into(),
            branch_meta_relpath: "branch-meta.json".into(),
        })
        .unwrap(),
    )
    .unwrap();
    let lease = acquire_exclusive_for_profile(&profile_root, "test export owner").unwrap();

    let report =
        export_profile_store_with_lease(&profile_root, project_id, &target, &lease).unwrap();

    assert_eq!(report.project_id, project_id);
    assert_eq!(fs::read(target.join("tracedecay.db")).unwrap(), b"database");
}

#[tokio::test]
async fn apply_migration_manifest_requires_exclusive_source_profile_lifecycle() {
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let project = root.join("repo");
    let data_dir = project.join(".tracedecay");
    let destination_profile = root.join("destination-profile");
    let graph_db = data_dir.join("tracedecay.db");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(&graph_db, b"graph").unwrap();
    let mut inventory = empty_inventory();
    inventory.stores.push(StoreInventory {
        project_root: project,
        data_dir: data_dir.clone(),
        db_path: graph_db.clone(),
        brand: StoreBrand::TraceDecay,
        role: StoreRole::CodeProjectStore,
        registry_status: RegistryStatus::Unregistered,
        size_bytes: 5,
        statuses: vec![StoreStatus::Ok],
        artifacts: vec![StoreArtifact {
            kind: "graph_db".to_string(),
            path: graph_db,
            size_bytes: 5,
        }],
    });
    let mut manifest = build_plan_manifest(
        inventory,
        MigrationPlanOptions {
            manifest_path: root.join("manifest.json"),
            migration_id: "mig_source_lock".to_string(),
            tracedecay_version: "0.0.2".to_string(),
            created_at_unix: 1_800_000_000,
            confirmation_token: "confirm-mig_source_lock".to_string(),
            target_profile_root: destination_profile.clone(),
            project_id: "proj_123".to_string(),
        },
    )
    .unwrap();
    save_manifest(&manifest).unwrap();
    let _lease = acquire_exclusive_for_profile(&data_dir, "source profile owner").unwrap();

    let error = apply_migration_manifest(&mut manifest).await.unwrap_err();

    assert!(error.to_string().contains("source profile owner"));
    assert!(!destination_profile.join("projects/proj_123").exists());
}

#[test]
fn cleanup_sources_requires_exclusive_profile_lifecycle_cutover() {
    let dir = TempDir::new().unwrap();
    let profile_root = canonical_temp_path(&dir.path().join("profile"));
    let source = dir.path().join("repo/.tracedecay/tracedecay.db");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::create_dir_all(&profile_root).unwrap();
    fs::write(&source, b"database").unwrap();
    let mut manifest = manifest_for(
        MigrationProtocol::for_manifest(dir.path().join("manifest.json"), "mig_123"),
        "mig_123",
    );
    manifest.destination.profile_root = Some(profile_root.clone());
    manifest.artifacts.push(MigrationArtifact {
        kind: "graph_db".to_string(),
        source_path: source.clone(),
        target_path: Some(profile_root.join("projects/proj_123/tracedecay.db")),
        state: ArtifactState::Applied,
    });
    let _lease = acquire_exclusive_for_profile(&profile_root, "test cleanup owner").unwrap();

    let error = cleanup_migration_sources(&manifest).unwrap_err();

    assert!(error.to_string().contains("test cleanup owner"));
    assert_eq!(fs::read(source).unwrap(), b"database");
}

#[test]
fn rollback_rejects_partial_apply_state() {
    let dir = TempDir::new().unwrap();
    let protocol = MigrationProtocol::for_manifest(dir.path().join("manifest.json"), "mig_123");
    let mut manifest = MigrationManifest::new(
        "mig_123",
        "0.0.2",
        1_800_000_000,
        "confirm-mig_123",
        protocol,
        MigrationInventory {
            stores: Vec::new(),
            skipped: Vec::new(),
            global_db: None,
        },
    );
    manifest.artifacts.push(MigrationArtifact {
        kind: "graph_db".to_string(),
        source_path: dir.path().join("repo/.tracedecay/tracedecay.db"),
        target_path: Some(dir.path().join("profile/projects/proj_123/tracedecay.db")),
        state: ArtifactState::Verified,
    });
    manifest.artifacts.push(MigrationArtifact {
        kind: "sessions_db".to_string(),
        source_path: dir.path().join("repo/.tracedecay/sessions.db"),
        target_path: Some(dir.path().join("profile/projects/proj_123/sessions.db")),
        state: ArtifactState::Planned,
    });

    assert_eq!(
        assess_migration_rollback_state(&manifest),
        MigrationRollbackState::PartialApply
    );
    let err = rollback_migration_manifest(&mut manifest).unwrap_err();
    assert!(
        err.to_string().contains("partial apply"),
        "unexpected error: {err}"
    );
}

#[test]
fn rollback_rejects_cutover_incomplete_state() {
    let dir = TempDir::new().unwrap();
    let protocol = MigrationProtocol::for_manifest(dir.path().join("manifest.json"), "mig_123");
    let mut manifest = MigrationManifest::new(
        "mig_123",
        "0.0.2",
        1_800_000_000,
        "confirm-mig_123",
        protocol,
        MigrationInventory {
            stores: Vec::new(),
            skipped: Vec::new(),
            global_db: None,
        },
    );
    manifest.artifacts.push(MigrationArtifact {
        kind: "graph_db".to_string(),
        source_path: dir.path().join("repo/.tracedecay/tracedecay.db"),
        target_path: Some(dir.path().join("profile/projects/proj_123/tracedecay.db")),
        state: ArtifactState::Verified,
    });

    assert_eq!(
        assess_migration_rollback_state(&manifest),
        MigrationRollbackState::CutoverIncomplete
    );
    let err = rollback_migration_manifest(&mut manifest).unwrap_err();
    assert!(
        err.to_string().contains("cutover") && err.to_string().contains("incomplete"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn apply_and_backup_preserve_wal_only_rows_for_graph_and_sessions_families() {
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let project = root.join("repo");
    let data_dir = project.join(".tracedecay");
    let destination_profile = root.join("destination-profile");
    let graph_db_path = data_dir.join("tracedecay.db");
    let sessions_db_path = data_dir.join("sessions.db");
    fs::create_dir_all(&data_dir).unwrap();

    let graph_connection = rusqlite::Connection::open(&graph_db_path).unwrap();
    graph_connection
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA wal_autocheckpoint=0;
             CREATE TABLE wal_probe (value TEXT NOT NULL);
             INSERT INTO wal_probe VALUES ('graph-wal-row');
             CREATE TABLE scale_probe (value INTEGER NOT NULL);
             WITH RECURSIVE values_to_insert(value) AS (
                 SELECT 1 UNION ALL SELECT value + 1 FROM values_to_insert WHERE value < 50000
             ) INSERT INTO scale_probe SELECT value FROM values_to_insert;",
        )
        .unwrap();
    let sessions_connection = rusqlite::Connection::open(&sessions_db_path).unwrap();
    sessions_connection
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA wal_autocheckpoint=0;
             CREATE TABLE wal_probe (value TEXT NOT NULL);
             INSERT INTO wal_probe VALUES ('sessions-wal-row');",
        )
        .unwrap();
    assert!(sqlite_sidecar(&graph_db_path, "-wal").is_file());
    assert!(sqlite_sidecar(&graph_db_path, "-shm").is_file());
    assert!(sqlite_sidecar(&sessions_db_path, "-wal").is_file());
    assert!(sqlite_sidecar(&sessions_db_path, "-shm").is_file());

    let mut artifacts = Vec::new();
    for (kind, path) in [
        ("graph_db", &graph_db_path),
        ("sessions_db", &sessions_db_path),
    ] {
        artifacts.push(StoreArtifact {
            kind: kind.to_string(),
            path: path.clone(),
            size_bytes: fs::metadata(path).unwrap().len(),
        });
        for (suffix, sidecar_kind) in [
            ("-wal", format!("{kind}_wal")),
            ("-shm", format!("{kind}_shm")),
        ] {
            let sidecar = sqlite_sidecar(path, suffix);
            artifacts.push(StoreArtifact {
                kind: sidecar_kind,
                size_bytes: fs::metadata(&sidecar).unwrap().len(),
                path: sidecar,
            });
        }
    }
    let inventory = MigrationInventory {
        stores: vec![StoreInventory {
            project_root: project.clone(),
            data_dir: data_dir.clone(),
            db_path: graph_db_path.clone(),
            brand: StoreBrand::TraceDecay,
            role: StoreRole::CodeProjectStore,
            registry_status: RegistryStatus::Unregistered,
            size_bytes: 0,
            statuses: vec![StoreStatus::Ok],
            artifacts,
        }],
        skipped: Vec::new(),
        global_db: None,
    };
    let manifest_path = root.join("manifest.json");
    let mut manifest = build_plan_manifest(
        inventory,
        MigrationPlanOptions {
            manifest_path: manifest_path.clone(),
            migration_id: "mig_wal_family".to_string(),
            tracedecay_version: "0.0.2".to_string(),
            created_at_unix: 1_800_000_000,
            confirmation_token: "confirm-mig_wal_family".to_string(),
            target_profile_root: destination_profile.clone(),
            project_id: "proj_123".to_string(),
        },
    )
    .unwrap();
    assert_eq!(manifest.artifacts.len(), 2);
    assert!(
        manifest
            .artifacts
            .iter()
            .all(|artifact| !artifact.kind.ends_with("_wal") && !artifact.kind.ends_with("_shm"))
    );
    save_manifest(&manifest).unwrap();

    tokio::time::timeout(
        std::time::Duration::from_secs(15),
        apply_migration_manifest(&mut manifest),
    )
    .await
    .expect("SQLite migration regressed beyond the bounded snapshot/hash path")
    .unwrap();
    assert!(
        !destination_profile
            .join("migration-backups/mig_wal_family/.sqlite-snapshot-scratch")
            .exists(),
        "exact SQLite snapshot scratch must not outlive retained snapshots"
    );

    let target_root = destination_profile.join("projects/proj_123");
    let backup_root = destination_profile.join("migration-backups/mig_wal_family");
    for root in [&target_root, &backup_root] {
        for database in ["tracedecay.db", "sessions.db"] {
            assert!(root.join(database).is_file());
            assert!(!root.join(format!("{database}-wal")).exists());
            assert!(!root.join(format!("{database}-shm")).exists());
        }
    }
    assert_eq!(
        rusqlite::Connection::open_with_flags(
            target_root.join("tracedecay.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
        .query_row("SELECT COUNT(*) FROM scale_probe", (), |row| row
            .get::<_, i64>(0))
        .unwrap(),
        50_000
    );
    for root in [&target_root, &backup_root] {
        for (database, expected) in [
            ("tracedecay.db", "graph-wal-row"),
            ("sessions.db", "sessions-wal-row"),
        ] {
            let connection = rusqlite::Connection::open_with_flags(
                root.join(database),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            let quick_check = connection
                .query_row("PRAGMA quick_check", (), |row| row.get::<_, String>(0))
                .unwrap();
            assert_eq!(quick_check, "ok");
            let value = connection
                .query_row("SELECT value FROM wal_probe", (), |row| {
                    row.get::<_, String>(0)
                })
                .unwrap();
            assert_eq!(value, expected);
        }
    }

    // A crash can publish a durable file before persisting the next state. The
    // next apply must verify and advance those files instead of overwriting or
    // rejecting them.
    for artifact in &mut manifest.artifacts {
        if is_sqlite_test_artifact(&artifact.kind) {
            artifact.state = ArtifactState::Locked;
        }
    }
    for artifact in &mut manifest.backup_artifacts {
        if is_sqlite_test_artifact(&artifact.kind) {
            artifact.state = ArtifactState::Locked;
        }
    }
    save_manifest(&manifest).unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(15),
        apply_migration_manifest(&mut manifest),
    )
    .await
    .expect("crash recovery regressed beyond the bounded verify/hash path")
    .unwrap();
    assert!(
        !destination_profile
            .join("migration-backups/mig_wal_family/.sqlite-snapshot-scratch")
            .exists(),
        "resumed exact SQLite snapshot scratch must be revoked"
    );
    assert!(
        manifest
            .artifacts
            .iter()
            .chain(&manifest.backup_artifacts)
            .all(|artifact| !is_sqlite_test_artifact(&artifact.kind)
                || artifact.state == ArtifactState::Verified)
    );
}

fn is_sqlite_test_artifact(kind: &str) -> bool {
    matches!(kind, "graph_db" | "sessions_db" | "branch_graph_db")
}
