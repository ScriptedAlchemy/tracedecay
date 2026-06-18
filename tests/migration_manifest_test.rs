use std::fs;
use std::path::PathBuf;
use std::process::Command;

use tempfile::TempDir;
use tracedecay::migrate::inventory::{
    MigrationInventory, RegistryStatus, StoreArtifact, StoreBrand, StoreInventory, StoreRole,
    StoreStatus,
};
use tracedecay::migrate::manifest::{
    build_plan_manifest, load_manifest, save_manifest, verify_migration_manifest, ArtifactState,
    MigrationArtifact, MigrationManifest, MigrationPlanOptions, MigrationProtocol,
    MIGRATION_MANIFEST_SCHEMA_VERSION,
};
use tracedecay::storage::{
    StorageMode, StoreKind, StoreManifest, STORE_MANIFEST_FILENAME, STORE_MANIFEST_SCHEMA_VERSION,
};

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
    let manifest_path = dir.path().join("manifest.json");
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
fn migrate_apply_remains_fail_closed_for_valid_manifest() {
    let dir = TempDir::new().unwrap();
    let manifest_path = dir.path().join("manifest.json");
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
            "apply",
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
        stderr.contains("migrate apply is not enabled yet"),
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
    let manifest_path = dir.path().join("manifest.json");
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
        stderr.contains("migrate rollback is not enabled"),
        "stderr was: {stderr}"
    );
}
