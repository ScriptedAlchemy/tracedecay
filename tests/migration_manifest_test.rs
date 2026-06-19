use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(unix)]
use std::os::unix::fs::symlink;
use tempfile::TempDir;
use tracedecay::global_db::GlobalDb;
use tracedecay::migrate::inventory::{
    MigrationInventory, RegistryStatus, StoreArtifact, StoreBrand, StoreInventory, StoreRole,
    StoreStatus,
};
use tracedecay::migrate::manifest::{
    apply_migration_manifest, assess_migration_rollback_state, build_plan_manifest,
    cleanup_migration_sources, finalize_migration_apply, load_manifest,
    rollback_migration_manifest, save_manifest, verify_migration_manifest, ArtifactState,
    MigrationArtifact, MigrationManifest, MigrationPlanOptions, MigrationProtocol,
    MigrationRollbackState, MIGRATION_MANIFEST_SCHEMA_VERSION,
};
use tracedecay::storage::{
    read_enrollment_marker, read_store_manifest, StorageMode, StoreKind, StoreManifest,
    STORE_MANIFEST_FILENAME, STORE_MANIFEST_SCHEMA_VERSION,
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
    #[cfg(windows)]
    {
        path.to_path_buf()
    }
    #[cfg(not(windows))]
    {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    }
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
fn save_manifest_rejects_unsafe_migration_ids_before_deriving_temp_paths() {
    let dir = TempDir::new().unwrap();
    let manifest_path = dir.path().join("migration-manifest.json");
    let manifest = manifest_for(
        MigrationProtocol::for_manifest(&manifest_path, "../escape"),
        "../escape",
    );

    let err = save_manifest(&manifest).unwrap_err();

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(!dir.path().join(".migration-manifest.json...").exists());
    assert!(!dir.path().join("escape.tmp").exists());
    assert!(!dir.path().join("migration-manifest.json.lock").exists());
}

#[test]
fn save_manifest_keeps_existing_manifest_and_removes_lock_when_tmp_write_fails() {
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let manifest_path = root.join("migration-manifest.json");
    let protocol = MigrationProtocol::for_manifest(&manifest_path, "mig_123");
    let old = manifest_for(protocol.clone(), "mig_123");
    save_manifest(&old).unwrap();
    let before = fs::read(&manifest_path).unwrap();
    fs::create_dir(&protocol.temp_manifest_path).unwrap();
    let new = manifest_for(protocol.clone(), "mig_123");

    let err = save_manifest(&new).unwrap_err();

    assert_ne!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(fs::read(&manifest_path).unwrap(), before);
    assert!(!protocol.lock_path.exists());
}

#[test]
fn save_manifest_replaces_existing_manifest_via_tmp_rename() {
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let manifest_path = root.join("migration-manifest.json");
    let protocol = MigrationProtocol::for_manifest(&manifest_path, "mig_123");
    let old = manifest_for(protocol.clone(), "mig_123");
    save_manifest(&old).unwrap();
    let new = MigrationManifest::new(
        "mig_123",
        "0.0.3",
        1_800_000_001,
        "confirm-mig_123",
        protocol.clone(),
        empty_inventory(),
    );

    save_manifest(&new).unwrap();
    let loaded = load_manifest(&manifest_path).unwrap();

    assert_eq!(loaded.tracedecay_version, "0.0.3");
    assert_eq!(loaded.created_at_unix, 1_800_000_001);
    assert!(!protocol.temp_manifest_path.exists());
    assert!(!protocol.lock_path.exists());
}

#[test]
fn manifest_protocol_records_lock_and_atomic_temp_paths() {
    let manifest_path = PathBuf::from("/tmp/profile-migration/manifest.json");

    let protocol = MigrationProtocol::for_manifest(&manifest_path, "mig_123");

    assert_eq!(protocol.manifest_path, manifest_path);
    assert_eq!(
        protocol.lock_path,
        PathBuf::from("/tmp/profile-migration/manifest.json.lock")
    );
    assert_eq!(
        protocol.temp_manifest_path,
        PathBuf::from("/tmp/profile-migration/.manifest.json.mig_123.tmp")
    );
}

#[test]
fn manifest_records_confirmation_token_and_artifacts() {
    let inventory = MigrationInventory {
        stores: Vec::new(),
        skipped: Vec::new(),
        global_db: None,
    };
    let protocol = MigrationProtocol::for_manifest("/tmp/manifest.json", "mig_123");

    let manifest = MigrationManifest::new(
        "mig_123",
        "0.0.2",
        1_800_000_000,
        "confirm-mig_123",
        protocol,
        inventory,
    );

    assert_eq!(manifest.schema_version, MIGRATION_MANIFEST_SCHEMA_VERSION);
    assert_eq!(manifest.confirmation_token, "confirm-mig_123");
    assert!(manifest.artifacts.is_empty());
}

#[test]
fn migration_artifacts_follow_apply_state_order() {
    let mut artifact = MigrationArtifact::new(
        "graph_db",
        PathBuf::from("/old/tracedecay.db"),
        Some(PathBuf::from("/new/tracedecay.db")),
    );

    assert_eq!(artifact.state, ArtifactState::Planned);
    artifact.transition_to(ArtifactState::Locked).unwrap();
    artifact.transition_to(ArtifactState::Copied).unwrap();
    artifact.transition_to(ArtifactState::Verified).unwrap();
    artifact.transition_to(ArtifactState::Applied).unwrap();

    assert!(artifact.transition_to(ArtifactState::Planned).is_err());
}

#[test]
fn manifest_persistence_roundtrips_through_atomic_paths() {
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let manifest_path = root.join("manifest.json");
    let inventory = MigrationInventory {
        stores: Vec::new(),
        skipped: Vec::new(),
        global_db: None,
    };
    let protocol = MigrationProtocol::for_manifest(&manifest_path, "mig_123");
    let manifest = MigrationManifest::new(
        "mig_123",
        "0.0.2",
        1_800_000_000,
        "confirm-mig_123",
        protocol.clone(),
        inventory,
    );

    save_manifest(&manifest).unwrap();
    let loaded = load_manifest(&manifest_path).unwrap();

    assert_eq!(loaded.migration_id, "mig_123");
    assert_eq!(loaded.confirmation_token, "confirm-mig_123");
    assert!(protocol.manifest_path.is_file());
    assert!(!protocol.temp_manifest_path.exists());
    assert!(!protocol.lock_path.exists());
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
    assert!(manifest
        .artifacts
        .iter()
        .all(|artifact| artifact.state == ArtifactState::Planned));
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
    fs::write(
        data_root.join("branch-meta.json"),
        r#"{"default_branch":"main","branches":{}}"#,
    )
    .unwrap();
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

#[test]
fn apply_migration_manifest_stops_at_verified_before_cutover() {
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
    fs::write(&branch_meta, r#"{"default_branch":"main","branches":{}}"#).unwrap();
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
                        size_bytes: 39,
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

    apply_migration_manifest(&mut manifest).unwrap();

    assert!(manifest
        .artifacts
        .iter()
        .all(|artifact| artifact.state == ArtifactState::Verified));
    assert!(read_enrollment_marker(&project).unwrap().is_none());
    let verify = verify_migration_manifest(&manifest);
    assert!(verify.cutover_ready, "{:?}", verify.issues);
    assert!(!verify.apply_supported, "{:?}", verify.issues);
}

#[test]
fn finalize_migration_apply_marks_cutover_complete() {
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let manifest_path = root.join("manifest.json");
    let project = root.join("repo");
    let data_dir = project.join(".tracedecay");
    let graph_db = data_dir.join("tracedecay.db");
    let profile_root = root.join("profile");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(&graph_db, b"graph").unwrap();
    fs::write(
        data_dir.join("branch-meta.json"),
        r#"{"default_branch":"main","branches":{}}"#,
    )
    .unwrap();
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
    apply_migration_manifest(&mut manifest).unwrap();
    tracedecay::storage::write_enrollment_marker(
        &project,
        &tracedecay::storage::EnrollmentMarker {
            project_id: "proj_123".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();

    finalize_migration_apply(&mut manifest).unwrap();

    assert!(manifest
        .artifacts
        .iter()
        .all(|artifact| artifact.state == ArtifactState::Applied));
    assert!(verify_migration_manifest(&manifest).apply_supported);
}

#[test]
fn apply_migration_manifest_rejects_target_parent_escape() {
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

    let err = apply_migration_manifest(&mut manifest).unwrap_err();

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

#[test]
fn migrate_apply_copies_single_store_and_cuts_over_profile_shard() {
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let manifest_path = root.join("manifest.json");
    let project = root.join("repo");
    let data_dir = project.join(".tracedecay");
    let graph_db = data_dir.join("tracedecay.db");
    let sessions_db = data_dir.join("sessions.db");
    let branch_meta = data_dir.join("branch-meta.json");
    let profile_root = root.join("profile");
    let global_db_path = root.join("global/global.db");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(&graph_db, b"graph").unwrap();
    fs::write(&sessions_db, b"sessions").unwrap();
    fs::write(&branch_meta, r#"{"default_branch":"main","branches":{}}"#).unwrap();
    let manifest = build_plan_manifest(
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
                        size_bytes: 39,
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
    );
    let manifest = manifest.unwrap();
    save_manifest(&manifest).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_tracedecay"))
        .env("TRACEDECAY_GLOBAL_DB", &global_db_path)
        .env("TRACEDECAY_ENABLE_GLOBAL_DB", "1")
        .args([
            "migrate",
            "apply",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "--confirm-token",
            "confirm-mig_123",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(profile_root.join("projects/proj_123/tracedecay.db")).unwrap(),
        b"graph"
    );
    assert_eq!(
        fs::read(profile_root.join("projects/proj_123/sessions.db")).unwrap(),
        b"sessions"
    );
    let store_manifest =
        read_store_manifest(&profile_root.join("projects/proj_123/store_manifest.json")).unwrap();
    assert_eq!(store_manifest.project_id.as_deref(), Some("proj_123"));
    assert_eq!(store_manifest.storage_mode, StorageMode::ProfileSharded);
    let marker = read_enrollment_marker(&project).unwrap().unwrap();
    assert_eq!(marker.project_id, "proj_123");
    assert_eq!(marker.storage_mode, StorageMode::ProfileSharded);

    let applied = load_manifest(&manifest_path).unwrap();
    assert!(applied
        .artifacts
        .iter()
        .all(|artifact| artifact.state == ArtifactState::Applied));
    let backup_graph = profile_root.join("migration-backups/mig_123/tracedecay.db");
    assert_eq!(fs::read(&backup_graph).unwrap(), b"graph");
    assert!(applied
        .backup_artifacts
        .iter()
        .any(|artifact| artifact.kind == "graph_db"
            && artifact.target_path.as_deref() == Some(backup_graph.as_path())
            && artifact.state == ArtifactState::Verified));
    assert!(applied
        .artifacts
        .iter()
        .any(|artifact| artifact.kind == "store_manifest"));

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let resolution = runtime.block_on(async {
        GlobalDb::open_at(&global_db_path)
            .await
            .unwrap()
            .resolve_project_store_by_alias(&project)
            .await
            .unwrap()
    });
    assert_eq!(resolution.project.project_id, "proj_123");
    assert_eq!(resolution.store.storage_mode, "profile_sharded");
    assert!(resolution
        .artifacts
        .iter()
        .any(|artifact| artifact.artifact_kind == "store_manifest"));

    let verify = verify_migration_manifest(&applied);
    assert!(verify.apply_supported, "{:?}", verify.issues);
    assert!(verify.issues.is_empty(), "{:?}", verify.issues);
}

#[test]
fn migrate_rollback_fails_closed_when_targets_diverged_after_apply() {
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let manifest_path = root.join("manifest.json");
    let protocol = MigrationProtocol::for_manifest(&manifest_path, "mig_123");
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
    let target = root.join("profile/projects/proj_123/tracedecay.db");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, b"changed").unwrap();
    manifest.artifacts.push(MigrationArtifact {
        kind: "graph_db".to_string(),
        source_path: root.join("repo/.tracedecay/tracedecay.db"),
        target_path: Some(target),
        state: ArtifactState::Applied,
    });
    save_manifest(&manifest).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_tracedecay"))
        .args([
            "migrate",
            "rollback",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "--confirm-token",
            "confirm-mig_123",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("divergent target writes"),
        "stderr was: {stderr}"
    );
}

#[test]
fn migrate_reconstruct_reports_registry_plans_without_applying_them() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project = dir.path().join("repo");
    let data_root = profile_root.join("projects/proj_123");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&data_root).unwrap();
    fs::write(data_root.join("tracedecay.db"), b"graph").unwrap();
    fs::write(
        data_root.join("branch-meta.json"),
        r#"{"default_branch":"main","branches":{}}"#,
    )
    .unwrap();
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
    fs::write(
        data_root.join(STORE_MANIFEST_FILENAME),
        serde_json::to_string_pretty(&store_manifest).unwrap(),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_tracedecay"))
        .args([
            "migrate",
            "reconstruct",
            "--profile-root",
            profile_root.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["plans"].as_array().unwrap().len(), 1);
    assert_eq!(payload["plans"][0]["project"]["project_id"], "proj_123");
}

#[test]
fn migrate_rollback_remains_fail_closed_for_valid_manifest() {
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let manifest_path = root.join("manifest.json");
    let protocol = MigrationProtocol::for_manifest(&manifest_path, "mig_123");
    let manifest = MigrationManifest::new(
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
    save_manifest(&manifest).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_tracedecay"))
        .args([
            "migrate",
            "rollback",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "--confirm-token",
            "confirm-mig_123",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("migration has not been applied"),
        "stderr was: {stderr}"
    );
}
