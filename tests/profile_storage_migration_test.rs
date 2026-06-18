use std::fs;
use std::path::Path;

use tempfile::TempDir;
use tracedecay::branch_meta::{self, BranchMeta};
use tracedecay::global_db::{GlobalDb, GraphScopeUpsert, StoreArtifactUpsert, StoreInstanceUpsert};
use tracedecay::migrate::registry::{
    reconstruct_registry_from_store_manifest, scan_profile_store_manifests,
};
use tracedecay::storage::{
    StorageMode, StoreKind, StoreManifest, STORE_MANIFEST_FILENAME, STORE_MANIFEST_SCHEMA_VERSION,
};

async fn table_exists(db_path: &std::path::Path, table: &str) -> bool {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            libsql::params![table],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().is_some()
}

fn write_profile_store_manifest(profile_root: &Path, project_root: &Path) -> std::path::PathBuf {
    let data_root = profile_root.join("projects/proj_123");
    fs::create_dir_all(&data_root).unwrap();
    fs::create_dir_all(project_root).unwrap();
    fs::write(data_root.join("tracedecay.db"), b"graph").unwrap();
    fs::write(data_root.join("sessions.db"), b"sessions").unwrap();
    let branch_meta = BranchMeta::new_for_dir(&data_root, "main");
    branch_meta::save_branch_meta(&data_root, &branch_meta).unwrap();
    let manifest = StoreManifest {
        schema_version: STORE_MANIFEST_SCHEMA_VERSION,
        project_id: Some("proj_123".to_string()),
        store_kind: StoreKind::CodeProject,
        storage_mode: StorageMode::ProfileSharded,
        project_root: project_root.to_path_buf(),
        data_root: data_root.clone(),
        graph_db_relpath: "tracedecay.db".into(),
        sessions_db_relpath: "sessions.db".into(),
        branch_meta_relpath: "branch-meta.json".into(),
    };
    let manifest_path = data_root.join(STORE_MANIFEST_FILENAME);
    fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    manifest_path
}

#[tokio::test]
async fn global_db_creates_profile_storage_registry_tables() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    GlobalDb::open_at(&db_path).await.unwrap();

    for table in [
        "code_projects",
        "project_aliases",
        "store_instances",
        "graph_scopes",
        "store_artifacts",
    ] {
        assert!(table_exists(&db_path, table).await, "{table} missing");
    }
}

#[test]
fn reconstructs_registry_records_from_profile_store_manifest() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    let manifest_path = write_profile_store_manifest(&profile_root, &project_root);

    let report =
        reconstruct_registry_from_store_manifest(&manifest_path, &profile_root, 1_800_000_001);

    assert!(report.issues.is_empty(), "{:?}", report.issues);
    assert_eq!(report.plans.len(), 1);
    let plan = &report.plans[0];
    assert_eq!(plan.project.project_id, "proj_123");
    assert_eq!(plan.project.project_root, project_root);
    assert_eq!(plan.project.aliases, vec![project_root]);
    assert_eq!(plan.store.project_id, "proj_123");
    assert_eq!(plan.store.store_kind, "code_project");
    assert_eq!(plan.store.storage_mode, "profile_sharded");
    assert_eq!(plan.store.store_relpath, "projects/proj_123");
    assert_eq!(
        plan.store.manifest_relpath.as_deref(),
        Some("projects/proj_123/store_manifest.json")
    );
    assert_eq!(plan.store.last_verified_at, Some(1_800_000_001));
    assert!(plan
        .artifacts
        .iter()
        .any(|artifact| artifact.artifact_kind == "graph_db"
            && artifact.relpath == "projects/proj_123/tracedecay.db"));
    assert!(plan
        .artifacts
        .iter()
        .any(|artifact| artifact.artifact_kind == "store_manifest"
            && artifact.relpath == "projects/proj_123/store_manifest.json"));
    assert_eq!(plan.graph_scopes.len(), 1);
    assert_eq!(plan.graph_scopes[0].branch_name, "main");
    assert_eq!(
        plan.graph_scopes[0].db_relpath,
        "projects/proj_123/tracedecay.db"
    );
}

#[test]
fn scan_profile_store_manifests_rejects_unsafe_manifest_relpaths() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let data_root = profile_root.join("projects/proj_bad");
    let project_root = dir.path().join("repo");
    fs::create_dir_all(&data_root).unwrap();
    fs::create_dir_all(&project_root).unwrap();
    let manifest = StoreManifest {
        schema_version: STORE_MANIFEST_SCHEMA_VERSION,
        project_id: Some("proj_bad".to_string()),
        store_kind: StoreKind::CodeProject,
        storage_mode: StorageMode::ProfileSharded,
        project_root,
        data_root,
        graph_db_relpath: "../outside.db".into(),
        sessions_db_relpath: "sessions.db".into(),
        branch_meta_relpath: "branch-meta.json".into(),
    };
    fs::write(
        profile_root.join("projects/proj_bad/store_manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let report = scan_profile_store_manifests(&profile_root, 1_800_000_001);

    assert!(report.plans.is_empty());
    assert!(report
        .issues
        .iter()
        .any(|issue| issue.contains("unsafe graph_db_relpath")));
}

#[tokio::test]
async fn registry_resolves_project_store_by_canonical_alias() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let project_root = dir.path().join("repo");
    fs::create_dir_all(&project_root).unwrap();
    let db = GlobalDb::open_at(&db_path).await.unwrap();

    let project = db
        .upsert_code_project(
            "proj_123",
            &project_root,
            None,
            Some("https://example.test/repo.git"),
            Some("main"),
        )
        .await
        .unwrap();
    db.upsert_project_alias(&project_root.join("."), &project.project_id)
        .await
        .unwrap();
    let store = db
        .upsert_store_instance(StoreInstanceUpsert {
            store_id: "store_123".to_string(),
            project_id: project.project_id.clone(),
            store_kind: "code_project".to_string(),
            storage_mode: "profile_sharded".to_string(),
            store_relpath: "projects/proj_123".to_string(),
            manifest_relpath: Some("projects/proj_123/store_manifest.json".to_string()),
            last_verified_at: Some(42),
            last_write_at: Some(43),
        })
        .await
        .unwrap();
    db.upsert_graph_scope(GraphScopeUpsert {
        graph_scope_id: "scope_123".to_string(),
        project_id: project.project_id.clone(),
        store_id: store.store_id.clone(),
        branch_name: "main".to_string(),
        db_relpath: "tracedecay.db".to_string(),
        parent_scope_id: None,
        last_synced_at: Some(44),
        writable: true,
    })
    .await
    .unwrap();
    db.upsert_store_artifact(StoreArtifactUpsert {
        store_id: store.store_id.clone(),
        artifact_kind: "graph_db".to_string(),
        relpath: "tracedecay.db".to_string(),
        size_bytes: Some(128),
        schema_version: Some("1".to_string()),
        updated_at: Some(45),
    })
    .await
    .unwrap();

    let resolved = db
        .resolve_project_store_by_alias(&project_root)
        .await
        .unwrap();

    assert_eq!(resolved.project.project_id, "proj_123");
    assert_eq!(resolved.store.store_id, "store_123");
    assert_eq!(resolved.graph_scopes.len(), 1);
    assert_eq!(resolved.graph_scopes[0].branch_name, "main");
    assert_eq!(resolved.artifacts.len(), 1);
    assert_eq!(resolved.artifacts[0].artifact_kind, "graph_db");
    assert_eq!(
        resolved.project.canonical_root,
        project_root.canonicalize().unwrap().to_string_lossy()
    );
}

#[tokio::test]
async fn delete_project_uses_same_canonical_key_as_upsert() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let project_root = dir.path().join("repo");
    fs::create_dir_all(&project_root).unwrap();
    let db = GlobalDb::open_at(&db_path).await.unwrap();

    db.upsert(&project_root, 99).await;
    assert_eq!(db.get_project_tokens(&project_root).await, 99);

    db.delete_project(&project_root.join(".")).await;

    assert_eq!(db.get_project_tokens(&project_root).await, 0);
}
