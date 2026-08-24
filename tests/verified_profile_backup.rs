//! End-to-end verified backup and restore over real database engines:
//! a profile holding a live SQLite global database and a Grafeo project
//! graph store is backed up as verified snapshot artifacts, rehearsed into
//! an isolated restore root, and the restored databases must open and accept
//! new work.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tracedecay::profile_backup::{
    ProfileBackupError, create_complete_profile_backup, rehearse_complete_profile_backup,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphDbRegistration, GraphDbRegistry, GraphDbRegistryConfig, GraphEntity,
    GraphEntityId, GraphMutation, GraphNamespace, GraphProjectionId, GraphTraversalDirection,
    GraphWatermark, GraphWriteBatch, NeverCancelled, SourceGeneration, TraversalRequest,
};
use tracedecay_runtime_core::storage::{
    STORE_MANIFEST_FILENAME, STORE_MANIFEST_SCHEMA_VERSION, StorageMode, StoreKind, StoreManifest,
    write_store_manifest_to_path,
};
use tracedecay_store::{
    BrainId, ProjectId, RetainedGraphStoreLeaseV1, StoreAuthorityEpochV1, StoreIncarnationV1,
    StoreRuntimeBindingV1, UserProfileId, VerifiedStoreLocatorV1, canonical_store_locator_digest,
};

const BRAIN_ID: &str = "brain.backup-verified";
const PROFILE_ID: &str = "profile.backup-verified";
const PROJECT_ID: &str = "project.backup-verified";
const GRAPH_DB_FILENAME: &str = "code.grafeo";

#[derive(Debug)]
struct HarnessGraphLease {
    binding: StoreRuntimeBindingV1,
    verified_locator: VerifiedStoreLocatorV1,
    canonical_path: PathBuf,
}

impl RetainedGraphStoreLeaseV1 for HarnessGraphLease {
    fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.verified_locator
    }

    fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }
}

fn live() -> Arc<dyn GraphCancellation> {
    Arc::new(NeverCancelled)
}

fn registration(graph_path: &Path) -> GraphDbRegistration {
    let binding = StoreRuntimeBindingV1::new(
        tracedecay_store::StoreShardIdV1::project(
            BrainId::try_from(BRAIN_ID.to_owned()).unwrap(),
            UserProfileId::try_from(PROFILE_ID.to_owned()).unwrap(),
            ProjectId::try_from(PROJECT_ID.to_owned()).unwrap(),
        ),
        StoreIncarnationV1::new(1).unwrap(),
        StoreAuthorityEpochV1::new(1).unwrap(),
    );
    let verified_locator = VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        binding.incarnation,
        canonical_store_locator_digest(graph_path).unwrap(),
    );
    GraphDbRegistration {
        authority_lease: Arc::new(HarnessGraphLease {
            binding,
            verified_locator,
            canonical_path: graph_path.to_path_buf(),
        }),
        cancellation: live(),
        lifecycle_cancellation: live(),
        deadline: Instant::now() + Duration::from_secs(30),
    }
}

fn seed_graph_store(graph_path: &Path) {
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
    let database = registry.resolve(registration(graph_path)).unwrap();
    database
        .apply_unverified(
            GraphWriteBatch::new(
                GraphNamespace::new("code-graph").unwrap(),
                GraphProjectionId::new("code").unwrap(),
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
    drop(database);
    assert!(registry.close(&registration(graph_path)).unwrap());
}

fn assert_graph_store_serves_seeded_entity(graph_path: &Path) {
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
    let database = registry.resolve(registration(graph_path)).unwrap();
    let result = database
        .traverse(TraversalRequest {
            namespace: GraphNamespace::new("code-graph").unwrap(),
            start: GraphEntityId::new("historical-symbol").unwrap(),
            relation_kinds: BTreeSet::new(),
            direction: GraphTraversalDirection::Outgoing,
            max_depth: 4,
            max_visits: 16,
            max_results: 16,
            cancellation: live(),
        })
        .unwrap();
    assert_eq!(result.visits.len(), 1);
    assert_eq!(result.visits[0].entity.as_str(), "historical-symbol");
    drop(database);
    assert!(registry.close(&registration(graph_path)).unwrap());
}

fn seed_profile(temp: &TempDir) -> (PathBuf, PathBuf) {
    let profile = temp.path().join("profile");
    let project_root = temp.path().join("project");
    let store = profile.join("projects").join(PROJECT_ID);
    fs::create_dir_all(&store).unwrap();
    fs::create_dir(&project_root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(&profile, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let identity_path = profile.join("profile-identity.json");
    fs::write(
        &identity_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "brain_id": BRAIN_ID,
            "profile_id": PROFILE_ID,
        }))
        .unwrap(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(&identity_path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    for (name, value) in [("enrollment.json", "{}"), ("config.toml", "[profile]\n")] {
        fs::write(profile.join(name), value).unwrap();
    }
    fs::create_dir(profile.join("migration-inventory")).unwrap();
    fs::write(
        profile.join("migration-inventory/current.json"),
        br#"{"schema":"final"}"#,
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
             VALUES ('historical-1', 'before backup');",
        )
        .unwrap();
    drop(global);

    for path in [
        profile.join("user-sessions.db"),
        profile.join("user-memory.db"),
        store.join("sessions.db"),
    ] {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch("CREATE TABLE durable_marker (value TEXT NOT NULL);")
            .unwrap();
    }

    seed_graph_store(&store.join(GRAPH_DB_FILENAME));

    // A pending retention journal must ride through backup and restore
    // byte-exact so the retention pass can replay it after a restore.
    fs::write(
        store.join("retention-journal.json"),
        br#"{"pending":"generation-sweep"}"#,
    )
    .unwrap();

    fs::write(store.join("branch-meta.json"), "{}").unwrap();
    write_store_manifest_to_path(
        &store.join(STORE_MANIFEST_FILENAME),
        &StoreManifest {
            schema_version: STORE_MANIFEST_SCHEMA_VERSION,
            project_id: Some(PROJECT_ID.to_owned()),
            store_kind: StoreKind::CodeProject,
            storage_mode: StorageMode::ProfileSharded,
            project_root: project_root.clone(),
            data_root: store,
            graph_db_relpath: GRAPH_DB_FILENAME.into(),
            sessions_db_relpath: "sessions.db".into(),
            branch_meta_relpath: "branch-meta.json".into(),
        },
    )
    .unwrap();
    (profile, project_root)
}

fn create_backup(temp: &TempDir, profile: &Path) -> PathBuf {
    let lease = tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(
        profile,
        "verified backup test",
    )
    .unwrap();
    create_complete_profile_backup(
        profile,
        &temp.path().join("backups"),
        "backup.verified",
        100,
        &lease,
    )
    .unwrap()
}

#[test]
fn verified_backup_restores_databases_that_open_and_accept_writes() {
    let temp = TempDir::new().unwrap();
    let (profile, _) = seed_profile(&temp);
    let backup = create_backup(&temp, &profile);

    let manifest = tracedecay::profile_backup::load_and_verify_backup(&backup).unwrap();
    assert_eq!(manifest.source_brain_id, BRAIN_ID);
    assert_eq!(manifest.source_profile_id, PROFILE_ID);
    assert_eq!(manifest.projects.len(), 1);
    assert_eq!(manifest.projects[0].project_id, PROJECT_ID);
    assert!(manifest.entries.iter().all(|entry| {
        !entry.logical_path.ends_with("-wal")
            && !entry.logical_path.ends_with("-shm")
            && !entry.logical_path.contains(".grafeo.wal")
    }));

    let restored = temp.path().join("restored");
    rehearse_complete_profile_backup(&backup, &restored).unwrap();

    // The restored global database is a self-contained snapshot that serves
    // historical rows and accepts new writes.
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
        "before backup"
    );
    connection
        .execute(
            "INSERT INTO host_history(event_id, payload) VALUES (?1, ?2)",
            ("historical-2", "after restore"),
        )
        .unwrap();

    // The restored graph store opens through the registry authority and
    // serves the fenced snapshot.
    assert_graph_store_serves_seeded_entity(
        &restored
            .join("projects")
            .join(PROJECT_ID)
            .join(GRAPH_DB_FILENAME),
    );

    // Retention journal state rides through byte-exact for post-restore replay.
    assert_eq!(
        fs::read(
            restored
                .join("projects")
                .join(PROJECT_ID)
                .join("retention-journal.json")
        )
        .unwrap(),
        br#"{"pending":"generation-sweep"}"#
    );
}

#[test]
fn rehearsal_rejects_a_graph_artifact_that_fails_engine_verification() {
    let temp = TempDir::new().unwrap();
    let (profile, _) = seed_profile(&temp);
    let backup = create_backup(&temp, &profile);
    let artifact_logical = format!("projects/{PROJECT_ID}/{GRAPH_DB_FILENAME}");
    let artifact_path = backup.join(&artifact_logical);

    // Corrupt the graph artifact and rewrite its manifest entry so the byte
    // inventory still matches: only engine-level verification can catch it.
    let corrupt_bytes = b"corrupt graph artifact".to_vec();
    fs::write(&artifact_path, &corrupt_bytes).unwrap();
    let manifest_path = backup.join("backup-manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    for entry in manifest["entries"].as_array_mut().unwrap() {
        if entry["logical_path"] == artifact_logical.as_str() {
            entry["byte_len"] = serde_json::Value::from(corrupt_bytes.len() as u64);
            entry["sha256"] = serde_json::Value::from(hex::encode(Sha256::digest(&corrupt_bytes)));
        }
    }
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let restored = temp.path().join("restored");

    let error = rehearse_complete_profile_backup(&backup, &restored).unwrap_err();

    assert!(
        matches!(&error, ProfileBackupError::CorruptBackup { message }
            if message.contains("verify restored graph store")),
        "unexpected error: {error}"
    );
    assert!(!restored.exists());
}
