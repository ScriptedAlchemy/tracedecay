use std::path::Path;

use tempfile::TempDir;
use tokio::sync::Mutex;
use tracedecay::global_db::{GlobalDb, GraphScopeUpsert, StoreArtifactUpsert, StoreInstanceUpsert};

static GLOBAL_REGISTRY_TEST_LOCK: Mutex<()> = Mutex::const_new(());

async fn upsert_test_store(db: &GlobalDb, project_id: &str, store_id: &str) {
    db.upsert_store_instance(StoreInstanceUpsert {
        store_id: store_id.to_string(),
        project_id: project_id.to_string(),
        store_kind: "code_project".to_string(),
        storage_mode: "profile_sharded".to_string(),
        store_relpath: format!("projects/{project_id}"),
        manifest_relpath: Some(format!("projects/{project_id}/store_manifest.json")),
        last_verified_at: Some(100),
        last_write_at: Some(101),
    })
    .await
    .unwrap();
}

async fn upsert_registry_fixture(db: &GlobalDb, project_root: &Path) {
    let project = db
        .upsert_code_project(
            "proj_registry",
            project_root,
            Some(&project_root.join(".git")),
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
            store_id: "store_registry".to_string(),
            project_id: project.project_id.clone(),
            store_kind: "code_project".to_string(),
            storage_mode: "profile_sharded".to_string(),
            store_relpath: "projects/proj_registry".to_string(),
            manifest_relpath: Some("projects/proj_registry/store_manifest.json".to_string()),
            last_verified_at: Some(100),
            last_write_at: Some(101),
        })
        .await
        .unwrap();
    db.upsert_graph_scope(GraphScopeUpsert {
        graph_scope_id: "scope_registry_main".to_string(),
        project_id: project.project_id,
        store_id: store.store_id.clone(),
        branch_name: "main".to_string(),
        db_relpath: "projects/proj_registry/tracedecay.db".to_string(),
        parent_scope_id: None,
        last_synced_at: Some(102),
        writable: true,
    })
    .await
    .unwrap();
    db.upsert_store_artifact(StoreArtifactUpsert {
        store_id: store.store_id,
        artifact_kind: "store_manifest".to_string(),
        relpath: "projects/proj_registry/store_manifest.json".to_string(),
        size_bytes: Some(2048),
        schema_version: Some("1".to_string()),
        updated_at: Some(103),
    })
    .await
    .unwrap();
}

async fn table_exists(db_path: &Path, table: &str) -> bool {
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

async fn project_column_exists(db_path: &Path, column: &str) -> bool {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn.query("PRAGMA table_info(projects)", ()).await.unwrap();
    while let Some(row) = rows.next().await.unwrap() {
        let name: String = row.get(1).unwrap();
        if name == column {
            return true;
        }
    }
    false
}

#[tokio::test]
async fn open_at_migrates_existing_project_rows_to_canonical_keys() {
    let _guard = GLOBAL_REGISTRY_TEST_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let project_root = dir.path().join("repo");
    std::fs::create_dir_all(&project_root).unwrap();
    let legacy_key = project_root.join(".").to_string_lossy().to_string();

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let raw_conn = raw_db.connect().unwrap();
    raw_conn
        .execute_batch(
            "CREATE TABLE projects (
                path TEXT PRIMARY KEY,
                tokens_saved INTEGER NOT NULL DEFAULT 0
            );",
        )
        .await
        .unwrap();
    raw_conn
        .execute(
            "INSERT INTO projects (path, tokens_saved) VALUES (?1, ?2)",
            libsql::params![legacy_key.as_str(), 77_i64],
        )
        .await
        .unwrap();
    drop(raw_conn);
    drop(raw_db);

    let db = GlobalDb::open_at(&db_path).await.unwrap();

    assert_eq!(db.get_project_tokens(&project_root).await, 77);
    assert_eq!(
        db.list_project_paths().await,
        vec![project_root.canonicalize().unwrap().to_string_lossy()]
    );
}

#[tokio::test]
async fn delete_project_paths_use_same_canonical_key_as_upsert() {
    let _guard = GLOBAL_REGISTRY_TEST_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let project_one = dir.path().join("repo-one");
    let project_two = dir.path().join("repo-two");
    std::fs::create_dir_all(&project_one).unwrap();
    std::fs::create_dir_all(&project_two).unwrap();
    let db = GlobalDb::open_at(&db_path).await.unwrap();

    db.upsert(&project_one, 10).await;
    db.upsert(&project_two, 20).await;
    db.delete_project(&project_one.join(".")).await;
    let deleted = db
        .delete_projects(&[project_two.join(".").to_string_lossy().to_string()])
        .await;

    assert_eq!(db.get_project_tokens(&project_one).await, 0);
    assert_eq!(deleted, 1);
    assert_eq!(db.get_project_tokens(&project_two).await, 0);
    assert_eq!(db.global_tokens_saved().await, Some(0));
}

#[tokio::test]
async fn upsert_preserves_highest_known_tokens_saved() {
    let _guard = GLOBAL_REGISTRY_TEST_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let project = dir.path().join("repo");
    std::fs::create_dir_all(&project).unwrap();
    let db = GlobalDb::open_at(&db_path).await.unwrap();

    db.upsert(&project, 12_007_312).await;
    db.upsert(&project.join("."), 0).await;
    assert_eq!(db.get_project_tokens(&project).await, 12_007_312);

    db.upsert(&project, 12_100_000).await;
    assert_eq!(db.get_project_tokens(&project).await, 12_100_000);
}

#[tokio::test]
async fn open_at_creates_registry_tables_and_round_trips_registry_records() {
    let _guard = GLOBAL_REGISTRY_TEST_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let project_root = dir.path().join("repo");
    std::fs::create_dir_all(&project_root).unwrap();
    let db = GlobalDb::open_at(&db_path).await.unwrap();

    for table in [
        "code_projects",
        "project_aliases",
        "store_instances",
        "graph_scopes",
        "store_artifacts",
    ] {
        assert!(table_exists(&db_path, table).await, "{table} missing");
    }

    upsert_registry_fixture(&db, &project_root).await;

    let projects = db.list_code_projects(10).await;
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].project_id, "proj_registry");
    assert_eq!(
        projects[0].canonical_root,
        project_root.canonicalize().unwrap().to_string_lossy()
    );
    assert_eq!(db.search_code_projects("repo.git", 10).await.len(), 1);

    let context = db
        .project_registry_context_by_alias(&project_root)
        .await
        .unwrap();
    assert_eq!(context.project.project_id, "proj_registry");
    let alias_paths: Vec<_> = context
        .aliases
        .iter()
        .map(|alias| alias.alias_path.as_str())
        .collect();
    let canonical_project_root = project_root
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert!(alias_paths.contains(&canonical_project_root.as_str()));
    assert!(alias_paths
        .iter()
        .any(|alias| alias.starts_with("git-common-dir:")));
    assert_eq!(context.stores.len(), 1);
    assert_eq!(context.stores[0].store.store_id, "store_registry");
    assert_eq!(context.stores[0].graph_scopes.len(), 1);
    assert_eq!(context.stores[0].graph_scopes[0].branch_name, "main");
    assert_eq!(context.stores[0].artifacts.len(), 1);
    assert_eq!(
        context.stores[0].artifacts[0].artifact_kind,
        "store_manifest"
    );
}

#[tokio::test]
async fn delete_code_projects_cascades_registry_rows_without_touching_legacy_projects() {
    let _guard = GLOBAL_REGISTRY_TEST_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let project_root = dir.path().join("repo");
    std::fs::create_dir_all(&project_root).unwrap();
    let db = GlobalDb::open_at(&db_path).await.unwrap();

    db.upsert(&project_root, 42).await;
    upsert_registry_fixture(&db, &project_root).await;

    let deleted = db
        .delete_code_projects(&["proj_registry".to_string()])
        .await;

    assert_eq!(deleted, 1);
    assert!(db.get_code_project("proj_registry").await.is_none());
    assert!(db
        .project_registry_context_by_id("proj_registry")
        .await
        .is_none());
    assert!(db.list_code_projects(10).await.is_empty());
    assert_eq!(db.get_project_tokens(&project_root).await, 42);
    assert_eq!(db.global_tokens_saved().await, Some(42));
}

#[tokio::test]
async fn registry_resolves_store_by_repo_identity_aliases() {
    let _guard = GLOBAL_REGISTRY_TEST_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .unwrap();
    let original = dir.path().join("original");
    let renamed = dir.path().join("renamed");
    let common_dir = dir.path().join("git/common");
    std::fs::create_dir_all(&original).unwrap();
    std::fs::create_dir_all(&renamed).unwrap();
    std::fs::create_dir_all(&common_dir).unwrap();

    db.upsert_code_project(
        "proj_repo_identity",
        &original,
        Some(&common_dir),
        Some("git@github.com:ScriptedAlchemy/tracedecay.git"),
        Some("main"),
    )
    .await
    .unwrap();
    upsert_test_store(&db, "proj_repo_identity", "store_repo_identity").await;

    let by_common_dir = db
        .resolve_project_store_by_identity(&renamed, Some(&common_dir))
        .await
        .unwrap();
    assert_eq!(by_common_dir.project.project_id, "proj_repo_identity");

    let by_remote = db
        .resolve_unique_project_store_by_git_remote(
            "https://github.com/ScriptedAlchemy/tracedecay.git",
        )
        .await
        .unwrap();
    assert_eq!(by_remote.store.store_id, "store_repo_identity");
}

#[tokio::test]
async fn registry_remote_resolution_is_conservative_when_ambiguous() {
    let _guard = GLOBAL_REGISTRY_TEST_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .unwrap();
    let one = dir.path().join("one");
    let two = dir.path().join("two");
    std::fs::create_dir_all(&one).unwrap();
    std::fs::create_dir_all(&two).unwrap();

    db.upsert_code_project(
        "proj_one",
        &one,
        None,
        Some("git@github.com:ScriptedAlchemy/tracedecay.git"),
        Some("main"),
    )
    .await
    .unwrap();
    upsert_test_store(&db, "proj_one", "store_one").await;
    db.upsert_code_project(
        "proj_two",
        &two,
        None,
        Some("https://github.com/ScriptedAlchemy/tracedecay"),
        Some("main"),
    )
    .await
    .unwrap();
    upsert_test_store(&db, "proj_two", "store_two").await;

    assert!(db
        .resolve_unique_project_store_by_git_remote(
            "https://github.com/ScriptedAlchemy/tracedecay.git"
        )
        .await
        .is_none());
}

#[tokio::test]
async fn legacy_projects_tokens_saved_schema_and_queries_still_work() {
    let _guard = GLOBAL_REGISTRY_TEST_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let project_one = dir.path().join("repo-one");
    let project_two = dir.path().join("repo-two");
    std::fs::create_dir_all(&project_one).unwrap();
    std::fs::create_dir_all(&project_two).unwrap();
    let db = GlobalDb::open_at(&db_path).await.unwrap();

    assert!(project_column_exists(&db_path, "path").await);
    assert!(project_column_exists(&db_path, "tokens_saved").await);

    db.upsert(&project_one, 11).await;
    db.upsert(&project_two, 22).await;
    db.upsert(&project_one.join("."), 33).await;

    assert_eq!(db.get_project_tokens(&project_one).await, 33);
    assert_eq!(db.get_project_tokens(&project_two.join(".")).await, 22);
    assert_eq!(db.global_tokens_saved().await, Some(55));
    assert_eq!(db.list_project_paths().await.len(), 2);
}
