use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use rusqlite::{Connection, OpenFlags};
use tempfile::TempDir;
use tracedecay_graph_db::{
    GraphCancellation, GraphDb, GraphDbLocation, GraphDbOpenOptions, GraphDurability, GraphEntity,
    GraphEntityId, GraphFormatVersion, GraphMutation, GraphNamespace, GraphProjectionId,
    GraphWatermark, GraphWriteBatch, NeverCancelled, SourceGeneration,
};
use tracedecay_migrate::profile_backup::{
    create_complete_profile_backup, rehearse_complete_profile_backup,
};
use tracedecay_runtime_core::storage::{
    STORE_MANIFEST_FILENAME, STORE_MANIFEST_SCHEMA_VERSION, StorageMode, StoreKind, StoreManifest,
    write_store_manifest_to_path,
};

const PROFILE_ID: &str = "profile.backup-final-v2";
const BRAIN_ID: &str = "brain.backup-final-v2";
const PROJECT_ID: &str = "project.backup-final-v2";

fn live() -> Arc<dyn GraphCancellation> {
    Arc::new(NeverCancelled)
}

fn graph_options(path: PathBuf) -> GraphDbOpenOptions {
    GraphDbOpenOptions {
        location: GraphDbLocation::Persistent(path),
        expected_format: GraphFormatVersion::new(2).unwrap(),
        durability: GraphDurability::Sync,
        cancellation: live(),
    }
}

fn seed_profile(temp: &TempDir) -> (PathBuf, PathBuf, PathBuf) {
    let profile = temp.path().join("profile");
    let project_root = temp.path().join("project");
    let store = profile.join("projects").join(PROJECT_ID);
    std::fs::create_dir_all(&store).unwrap();
    std::fs::create_dir(&project_root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&profile, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::write(
        profile.join("profile-identity.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "brain_id": BRAIN_ID,
            "profile_id": PROFILE_ID,
        }))
        .unwrap(),
    )
    .unwrap();
    for (name, value) in [("enrollment.json", "{}"), ("config.toml", "[profile]\n")] {
        std::fs::write(profile.join(name), value).unwrap();
    }
    std::fs::create_dir(profile.join("migration-inventory")).unwrap();
    std::fs::write(
        profile.join("migration-inventory/current.json"),
        br#"{"schema":"final-v2"}"#,
    )
    .unwrap();

    let global = Connection::open(profile.join("global.db")).unwrap();
    global
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE host_history (
               event_id TEXT PRIMARY KEY NOT NULL,
               payload TEXT NOT NULL
             );
             INSERT INTO host_history(event_id, payload)
             VALUES ('historical-1', 'before reset');",
        )
        .unwrap();
    global
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    drop(global);

    for path in [
        profile.join("user-sessions.db"),
        profile.join("user-memory.db"),
        store.join("sessions.db"),
    ] {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch("CREATE TABLE final_v2_marker (value TEXT NOT NULL);")
            .unwrap();
    }

    let graph_path = store.join("code.grafeo");
    let graph = GraphDb::open(graph_options(graph_path.clone())).unwrap();
    graph
        .apply(
            GraphWriteBatch::new(
                GraphNamespace::new("code-graph").unwrap(),
                GraphProjectionId::new("code-generation").unwrap(),
                SourceGeneration::new("generation-1").unwrap(),
                GraphWatermark::new("watermark-1").unwrap(),
                vec![GraphMutation::UpsertEntity(
                    GraphEntity::new(
                        GraphEntityId::new("historical-symbol").unwrap(),
                        BTreeSet::new(),
                        BTreeMap::new(),
                    )
                    .unwrap(),
                )],
                live(),
            )
            .unwrap(),
        )
        .unwrap();
    graph.close().unwrap();

    std::fs::write(store.join("branch-meta.json"), "{}").unwrap();
    write_store_manifest_to_path(
        &store.join(STORE_MANIFEST_FILENAME),
        &StoreManifest {
            schema_version: STORE_MANIFEST_SCHEMA_VERSION,
            project_id: Some(PROJECT_ID.to_owned()),
            store_kind: StoreKind::CodeProject,
            storage_mode: StorageMode::ProfileSharded,
            project_root: project_root.clone(),
            data_root: store,
            graph_db_relpath: "code.grafeo".into(),
            sessions_db_relpath: "sessions.db".into(),
            branch_meta_relpath: "branch-meta.json".into(),
        },
    )
    .unwrap();
    (profile, project_root, graph_path)
}

#[test]
fn final_v2_backup_restores_exact_identity_and_accepts_later_host_history() {
    let temp = TempDir::new().unwrap();
    let (profile, _, _) = seed_profile(&temp);
    let lease = tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(
        &profile,
        "final-v2 backup test",
    )
    .unwrap();
    let backup = create_complete_profile_backup(
        &profile,
        &temp.path().join("backups"),
        "backup.final-v2",
        100,
        &lease,
    )
    .unwrap();

    let manifest = tracedecay_migrate::profile_backup::load_and_verify_backup(&backup).unwrap();
    assert_eq!(manifest.source_brain_id, BRAIN_ID);
    assert_eq!(manifest.source_profile_id, PROFILE_ID);
    assert_eq!(manifest.projects.len(), 1);
    assert_eq!(manifest.projects[0].project_id, PROJECT_ID);
    assert!(manifest.entries.iter().all(
        |entry| !entry.logical_path.ends_with("-wal") && !entry.logical_path.ends_with("-shm")
    ));

    let restored = temp.path().join("restored");
    rehearse_complete_profile_backup(&backup, &restored).unwrap();

    let connection = Connection::open_with_flags(
        restored.join("global.db"),
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT payload FROM host_history WHERE event_id = 'historical-1'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "before reset"
    );
    connection
        .execute(
            "INSERT INTO host_history(event_id, payload) VALUES (?1, ?2)",
            ("historical-2", "after reset"),
        )
        .unwrap();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM host_history", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );

    let restored_graph = restored
        .join("projects")
        .join(PROJECT_ID)
        .join("code.grafeo");
    GraphDb::open(graph_options(restored_graph))
        .unwrap()
        .close()
        .unwrap();
}

#[test]
fn restore_rejects_profile_identity_tampering_before_publication() {
    let temp = TempDir::new().unwrap();
    let (profile, _, _) = seed_profile(&temp);
    let lease = tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(
        &profile,
        "final-v2 backup test",
    )
    .unwrap();
    let backup = create_complete_profile_backup(
        &profile,
        &temp.path().join("backups"),
        "backup.final-v2",
        100,
        &lease,
    )
    .unwrap();
    std::fs::write(
        backup.join("profile-identity.json"),
        br#"{"schema_version":1,"brain_id":"brain.foreign","profile_id":"profile.foreign"}"#,
    )
    .unwrap();
    let restored = temp.path().join("restored");

    let error = rehearse_complete_profile_backup(&backup, &restored).unwrap_err();

    assert!(error.contains("checksum mismatch"));
    assert!(!restored.exists());
}
