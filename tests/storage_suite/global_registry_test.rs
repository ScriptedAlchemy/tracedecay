use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;
use tokio::sync::Mutex;
use tracedecay::application::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay::global_db::{
    GraphScopeUpsert, ProjectObservationStoreError, StoreArtifactUpsert, StoreInstanceUpsert,
};
use tracedecay::storage::{
    BRANCH_META_FILENAME, SESSIONS_DB_FILENAME, STORE_MANIFEST_SCHEMA_VERSION, StorageMode,
    StoreKind, StoreManifest, write_store_manifest_to_path,
};

static GLOBAL_REGISTRY_TEST_LOCK: Mutex<()> = Mutex::const_new(());

async fn upsert_test_store(db: &HostAdmissionTestRuntimeV1, project_id: &str, store_id: &str) {
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

fn observation_store_upsert(
    project_id: &str,
    store_id: &str,
    last_verified_at: Option<i64>,
) -> StoreInstanceUpsert {
    StoreInstanceUpsert {
        store_id: store_id.to_string(),
        project_id: project_id.to_string(),
        store_kind: "code_project".to_string(),
        storage_mode: "profile_sharded".to_string(),
        store_relpath: format!("projects/{project_id}"),
        manifest_relpath: Some(format!("projects/{project_id}/store_manifest.json")),
        last_verified_at,
        last_write_at: Some(101),
    }
}

fn observation_store_paths(profile_root: &Path, project_id: &str) -> (PathBuf, PathBuf, PathBuf) {
    let store_root = profile_root.join(format!("projects/{project_id}"));
    let manifest_path = store_root.join("store_manifest.json");
    let database_path = store_root.join("sessions.db");
    (store_root, manifest_path, database_path)
}

fn write_observation_store_manifest(
    profile_root: &Path,
    project_root: &Path,
    project_id: &str,
) -> (PathBuf, PathBuf, PathBuf) {
    let paths = observation_store_paths(profile_root, project_id);
    fs::create_dir_all(&paths.0).unwrap();
    write_store_manifest_to_path(
        &paths.1,
        &StoreManifest {
            schema_version: STORE_MANIFEST_SCHEMA_VERSION,
            project_id: Some(project_id.to_string()),
            store_kind: StoreKind::CodeProject,
            storage_mode: StorageMode::ProfileSharded,
            project_root: project_root.to_path_buf(),
            data_root: paths.0.clone(),
            graph_db_relpath: PathBuf::from("tracedecay.db"),
            sessions_db_relpath: PathBuf::from(SESSIONS_DB_FILENAME),
            branch_meta_relpath: PathBuf::from(BRANCH_META_FILENAME),
        },
    )
    .unwrap();
    paths
}

async fn create_observation_store_artifacts(
    profile_root: &Path,
    project_root: &Path,
    project_id: &str,
) -> (PathBuf, PathBuf, PathBuf) {
    let paths = write_observation_store_manifest(profile_root, project_root, project_id);
    let (database, _) = crate::common::initialize_test_database(&paths.2)
        .await
        .unwrap();
    drop(database);
    paths
}

async fn close_profile_runtime(db: HostAdmissionTestRuntimeV1) {
    db.checkpoint_profile_database_for_test().await;
    drop(db);
}

async fn upsert_registry_fixture(db: &HostAdmissionTestRuntimeV1, project_root: &Path) {
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

fn project_column_exists(db_path: &Path, column: &str) -> bool {
    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let mut statement = conn.prepare("PRAGMA table_info(projects)").unwrap();
    statement
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .any(|name| name.is_ok_and(|name| name == column))
}

#[tokio::test]
async fn registered_profile_runtime_migrates_existing_project_rows_to_canonical_keys() {
    let _guard = GLOBAL_REGISTRY_TEST_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let project_root = dir.path().join("repo");
    std::fs::create_dir_all(&project_root).unwrap();
    let legacy_key = project_root.join(".").to_string_lossy().to_string();

    let raw_conn = rusqlite::Connection::open(&db_path).unwrap();
    raw_conn
        .execute_batch(
            "CREATE TABLE projects (
                path TEXT PRIMARY KEY,
                tokens_saved INTEGER NOT NULL DEFAULT 0
            );",
        )
        .unwrap();
    raw_conn
        .execute(
            "INSERT INTO projects (path, tokens_saved) VALUES (?1, ?2)",
            rusqlite::params![legacy_key.as_str(), 77_i64],
        )
        .unwrap();
    drop(raw_conn);

    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();

    assert_eq!(db.get_project_tokens(&project_root).await, 77);
    assert_eq!(
        db.list_project_paths_compat().await,
        vec![project_root.canonicalize().unwrap().to_string_lossy()]
    );
    close_profile_runtime(db).await;
}

#[tokio::test]
async fn delete_project_paths_use_same_canonical_key_as_upsert() {
    let _guard = GLOBAL_REGISTRY_TEST_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project_one = dir.path().join("repo-one");
    let project_two = dir.path().join("repo-two");
    std::fs::create_dir_all(&project_one).unwrap();
    std::fs::create_dir_all(&project_two).unwrap();
    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();

    db.upsert(&project_one, 10).await;
    db.upsert(&project_two, 20).await;
    db.delete_project(&project_one.join(".")).await;
    let deleted = db
        .delete_projects(&[project_two.join(".").to_string_lossy().into_owned()])
        .await;

    assert_eq!(db.get_project_tokens(&project_one).await, 0);
    assert_eq!(deleted, 1);
    assert_eq!(db.get_project_tokens(&project_two).await, 0);
    assert_eq!(db.global_tokens_saved().await, Some(0));
    close_profile_runtime(db).await;
}

#[tokio::test]
async fn upsert_preserves_highest_known_tokens_saved() {
    let _guard = GLOBAL_REGISTRY_TEST_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    std::fs::create_dir_all(&project).unwrap();
    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();

    db.upsert(&project, 12_007_312).await;
    db.upsert(&project.join("."), 0).await;
    assert_eq!(db.get_project_tokens(&project).await, 12_007_312);

    db.upsert(&project, 12_100_000).await;
    assert_eq!(db.get_project_tokens(&project).await, 12_100_000);
    close_profile_runtime(db).await;
}

#[tokio::test]
async fn registered_profile_runtime_creates_and_round_trips_registry_records() {
    let _guard = GLOBAL_REGISTRY_TEST_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project_root = dir.path().join("repo");
    std::fs::create_dir_all(&project_root).unwrap();
    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();
    db.validate_profile_registry_schema_contract_for_test()
        .await
        .unwrap();
    close_profile_runtime(db).await;

    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();
    db.validate_profile_registry_schema_contract_for_test()
        .await
        .unwrap();
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
        .unwrap()
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
    assert!(
        alias_paths
            .iter()
            .any(|alias| alias.starts_with("git-common-dir:"))
    );
    assert_eq!(context.stores.len(), 1);
    assert_eq!(context.stores[0].store.store_id, "store_registry");
    assert_eq!(context.stores[0].graph_scopes.len(), 1);
    assert_eq!(context.stores[0].graph_scopes[0].branch_name, "main");
    assert_eq!(context.stores[0].artifacts.len(), 1);
    assert_eq!(
        context.stores[0].artifacts[0].artifact_kind,
        "store_manifest"
    );
    close_profile_runtime(db).await;
}

#[tokio::test]
async fn delete_code_projects_cascades_registry_rows_without_touching_legacy_projects() {
    let _guard = GLOBAL_REGISTRY_TEST_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project_root = dir.path().join("repo");
    std::fs::create_dir_all(&project_root).unwrap();
    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();

    db.upsert(&project_root, 42).await;
    upsert_registry_fixture(&db, &project_root).await;

    let deleted = db
        .delete_code_projects(&["proj_registry".to_string()])
        .await;

    assert_eq!(deleted, 1);
    assert!(
        db.get_code_project("proj_registry")
            .await
            .expect("registry read for a deleted project should not fault")
            .is_none()
    );
    assert!(
        db.project_registry_context_by_id("proj_registry")
            .await
            .is_none()
    );
    assert!(db.list_code_projects(10).await.is_empty());
    assert_eq!(db.get_project_tokens(&project_root).await, 42);
    assert_eq!(db.global_tokens_saved().await, Some(42));
    close_profile_runtime(db).await;
}

#[tokio::test]
async fn registry_resolves_store_by_repo_identity_aliases() {
    let _guard = GLOBAL_REGISTRY_TEST_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
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
        .unwrap()
        .unwrap();
    assert_eq!(by_common_dir.project.project_id, "proj_repo_identity");

    let by_remote = db
        .resolve_unique_project_store_by_git_remote(
            "https://github.com/ScriptedAlchemy/tracedecay.git",
        )
        .await
        .unwrap();
    assert_eq!(by_remote.store.store_id, "store_repo_identity");
    close_profile_runtime(db).await;
}

#[tokio::test]
async fn registry_context_resolves_linked_worktree_by_git_common_dir_identity() {
    let _guard = GLOBAL_REGISTRY_TEST_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();
    let main_checkout = dir.path().join("main");
    let linked_worktree = dir.path().join("linked");
    let common_dir = dir.path().join("repo/.git");
    std::fs::create_dir_all(&main_checkout).unwrap();
    std::fs::create_dir_all(&linked_worktree).unwrap();
    std::fs::create_dir_all(&common_dir).unwrap();

    db.upsert_code_project(
        "proj_worktree",
        &main_checkout,
        Some(&common_dir),
        Some("git@github.com:ScriptedAlchemy/tracedecay.git"),
        Some("main"),
    )
    .await
    .unwrap();
    upsert_test_store(&db, "proj_worktree", "store_worktree").await;

    assert!(
        db.project_registry_context_by_alias(&linked_worktree)
            .await
            .unwrap()
            .is_none()
    );
    let context = db
        .project_registry_context_by_identity(&linked_worktree, Some(&common_dir))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(context.project.project_id, "proj_worktree");
    assert_eq!(context.stores[0].store.store_id, "store_worktree");
    close_profile_runtime(db).await;
}

#[tokio::test]
async fn observation_store_resolver_returns_canonical_registered_paths() {
    let _guard = GLOBAL_REGISTRY_TEST_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    fs::create_dir_all(&project_root).unwrap();
    let db = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .unwrap();
    let project_id = "proj_observation";
    db.upsert_code_project(project_id, &project_root, None, None, Some("main"))
        .await
        .unwrap();
    db.upsert_store_instance(observation_store_upsert(
        project_id,
        "store_observation",
        Some(100),
    ))
    .await
    .unwrap();
    let (store_root, _, database_path) =
        create_observation_store_artifacts(&profile_root, &project_root, project_id).await;

    let resolution = db
        .resolve_project_observation_store(&project_root.join("."))
        .await
        .unwrap();

    assert_eq!(resolution.project().project_id, project_id);
    assert_eq!(resolution.store().store_id, "store_observation");
    assert_eq!(resolution.store_root(), store_root.canonicalize().unwrap());
    assert_eq!(
        resolution.database_path(),
        database_path.canonicalize().unwrap()
    );
    close_profile_runtime(db).await;
}

#[tokio::test]
async fn observation_store_resolver_fails_closed_without_project_or_store() {
    let _guard = GLOBAL_REGISTRY_TEST_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    fs::create_dir_all(&project_root).unwrap();
    let db = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .unwrap();

    let error = db
        .resolve_project_observation_store(&project_root)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ProjectObservationStoreError::ProjectNotRegistered { project_root: root }
            if root == project_root.canonicalize().unwrap()
    ));

    db.upsert_code_project("proj_no_store", &project_root, None, None, None)
        .await
        .unwrap();
    let error = db
        .resolve_project_observation_store(&project_root)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ProjectObservationStoreError::StoreNotRegistered { project_id }
            if project_id == "proj_no_store"
    ));
    assert!(!profile_root.join("projects/proj_no_store").exists());
    close_profile_runtime(db).await;
}

#[tokio::test]
async fn observation_store_resolver_rejects_multiple_stores_without_newest_fallback() {
    let _guard = GLOBAL_REGISTRY_TEST_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    fs::create_dir_all(&project_root).unwrap();
    let db = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .unwrap();
    let project_id = "proj_ambiguous_stores";
    db.upsert_code_project(project_id, &project_root, None, None, None)
        .await
        .unwrap();
    db.upsert_store_instance(observation_store_upsert(
        project_id,
        "store_older",
        Some(100),
    ))
    .await
    .unwrap();
    db.upsert_store_instance(observation_store_upsert(
        project_id,
        "store_newer",
        Some(200),
    ))
    .await
    .unwrap();
    create_observation_store_artifacts(&profile_root, &project_root, project_id).await;

    let identity_error = db
        .resolve_project_store_by_identity(&project_root, None)
        .await
        .unwrap_err();
    assert!(
        identity_error
            .to_string()
            .contains("resolves to multiple stores")
    );
    assert!(
        db.resolve_project_store_by_alias(&project_root)
            .await
            .is_none(),
        "legacy resolver must not select a newest store from ambiguous authority"
    );

    let error = db
        .resolve_project_observation_store(&project_root)
        .await
        .unwrap_err();
    let (actual_project_id, mut store_ids) = match error {
        ProjectObservationStoreError::AmbiguousStores {
            project_id,
            store_ids,
        } => (project_id, store_ids),
        other => panic!("expected ambiguous stores error, got {other:?}"),
    };
    store_ids.sort();
    assert_eq!(actual_project_id, project_id);
    assert_eq!(
        store_ids,
        vec!["store_newer".to_string(), "store_older".to_string()]
    );
    close_profile_runtime(db).await;
}

#[tokio::test]
async fn observation_store_resolver_rejects_unverified_store_as_stale() {
    let _guard = GLOBAL_REGISTRY_TEST_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    fs::create_dir_all(&project_root).unwrap();
    let db = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .unwrap();
    let project_id = "proj_stale_store";
    let store_id = "store_stale";
    db.upsert_code_project(project_id, &project_root, None, None, None)
        .await
        .unwrap();
    db.upsert_store_instance(observation_store_upsert(project_id, store_id, None))
        .await
        .unwrap();
    create_observation_store_artifacts(&profile_root, &project_root, project_id).await;

    let error = db
        .resolve_project_observation_store(&project_root)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ProjectObservationStoreError::StaleStore {
            project_id: actual_project_id,
            store_id: actual_store_id,
        } if actual_project_id == project_id && actual_store_id == store_id
    ));
    close_profile_runtime(db).await;
}

#[tokio::test]
async fn observation_store_resolver_requires_exact_canonical_store_metadata() {
    let _guard = GLOBAL_REGISTRY_TEST_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    fs::create_dir_all(&project_root).unwrap();
    let db = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .unwrap();
    let project_id = "proj_store_shape";
    let store_id = "store_shape";
    db.upsert_code_project(project_id, &project_root, None, None, None)
        .await
        .unwrap();
    create_observation_store_artifacts(&profile_root, &project_root, project_id).await;
    let canonical = observation_store_upsert(project_id, store_id, Some(100));
    let cases = [
        (
            "store_kind",
            StoreInstanceUpsert {
                store_kind: "session_store".to_string(),
                ..canonical.clone()
            },
        ),
        (
            "storage_mode",
            StoreInstanceUpsert {
                storage_mode: "project_local".to_string(),
                ..canonical.clone()
            },
        ),
        (
            "store_relpath project id",
            StoreInstanceUpsert {
                store_relpath: "projects/proj_other".to_string(),
                ..canonical.clone()
            },
        ),
        (
            "store_relpath normalization",
            StoreInstanceUpsert {
                store_relpath: format!("projects/{project_id}/."),
                ..canonical.clone()
            },
        ),
        (
            "manifest_relpath presence",
            StoreInstanceUpsert {
                manifest_relpath: None,
                ..canonical.clone()
            },
        ),
        (
            "manifest_relpath exact value",
            StoreInstanceUpsert {
                manifest_relpath: Some(format!("projects/{project_id}/./store_manifest.json")),
                ..canonical
            },
        ),
    ];

    for (case, upsert) in cases {
        db.upsert_store_instance(upsert).await.unwrap();
        let error = db
            .resolve_project_observation_store(&project_root)
            .await
            .unwrap_err();
        match &error {
            ProjectObservationStoreError::NonCanonicalStore {
                project_id: actual_project_id,
                store_id: actual_store_id,
                reason,
            } => {
                assert_eq!(actual_project_id, project_id, "{case}");
                assert_eq!(actual_store_id, store_id, "{case}");
                assert!(!reason.is_empty(), "{case}");
            }
            other => panic!("{case} should reject as noncanonical, got {other:?}"),
        }
    }
    close_profile_runtime(db).await;
}

#[tokio::test]
async fn observation_store_resolver_requires_existing_artifacts_and_creates_nothing() {
    let _guard = GLOBAL_REGISTRY_TEST_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    fs::create_dir_all(&project_root).unwrap();
    let db = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .unwrap();
    let profile_root = profile_root.canonicalize().unwrap();
    let project_id = "proj_missing_artifacts";
    let store_id = "store_missing_artifacts";
    db.upsert_code_project(project_id, &project_root, None, None, None)
        .await
        .unwrap();
    db.upsert_store_instance(observation_store_upsert(project_id, store_id, Some(100)))
        .await
        .unwrap();
    let (store_root, manifest_path, database_path) =
        observation_store_paths(&profile_root, project_id);

    let error = db
        .resolve_project_observation_store(&project_root)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ProjectObservationStoreError::UnavailableStore { path, .. } if path == store_root
    ));
    assert!(
        !store_root.exists(),
        "resolver must not create the store root"
    );

    fs::create_dir_all(&store_root).unwrap();
    let error = db
        .resolve_project_observation_store(&project_root)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ProjectObservationStoreError::UnavailableStore { path, .. } if path == manifest_path
    ));
    assert!(
        !manifest_path.exists(),
        "resolver must not create the manifest"
    );

    fs::write(&manifest_path, b"{}").unwrap();
    assert!(matches!(
        db.resolve_project_observation_store(&project_root)
            .await
            .unwrap_err(),
        ProjectObservationStoreError::NonCanonicalStore { .. }
    ));

    write_observation_store_manifest(&profile_root, &project_root, project_id);
    let error = db
        .resolve_project_observation_store(&project_root)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ProjectObservationStoreError::UnavailableStore { path, .. } if path == database_path
    ));
    assert!(
        !database_path.exists(),
        "resolver must not create sessions.db"
    );

    fs::write(&database_path, b"not a database").unwrap();
    assert!(matches!(
        db.resolve_project_observation_store(&project_root)
            .await
            .unwrap_err(),
        ProjectObservationStoreError::NonCanonicalStore { .. }
    ));
    fs::remove_file(&database_path).unwrap();
    let (database, _) = crate::common::initialize_test_database(&database_path)
        .await
        .unwrap();
    drop(database);
    assert!(
        db.resolve_project_observation_store(&project_root)
            .await
            .is_ok()
    );
    close_profile_runtime(db).await;
}

#[tokio::test]
async fn observation_store_resolver_rejects_a_registered_but_missing_checkout() {
    let _guard = GLOBAL_REGISTRY_TEST_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    fs::create_dir_all(&project_root).unwrap();
    let db = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .unwrap();
    let project_id = "proj_missing_checkout";
    db.upsert_code_project(project_id, &project_root, None, None, None)
        .await
        .unwrap();
    db.upsert_store_instance(observation_store_upsert(
        project_id,
        "store_missing_checkout",
        Some(100),
    ))
    .await
    .unwrap();
    create_observation_store_artifacts(&profile_root, &project_root, project_id).await;
    fs::remove_dir_all(&project_root).unwrap();

    assert!(matches!(
        db.resolve_project_observation_store(&project_root)
            .await
            .unwrap_err(),
        ProjectObservationStoreError::UnavailableProject { project_root: missing }
            if missing == project_root
    ));
    close_profile_runtime(db).await;
}

#[cfg(unix)]
#[tokio::test]
async fn observation_store_resolver_rejects_symlinked_store_artifacts() {
    use std::os::unix::fs::symlink;

    let _guard = GLOBAL_REGISTRY_TEST_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    fs::create_dir_all(&project_root).unwrap();
    let db = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .unwrap();
    let project_id = "proj_symlinked_store";
    let store_id = "store_symlinked";
    db.upsert_code_project(project_id, &project_root, None, None, None)
        .await
        .unwrap();
    db.upsert_store_instance(observation_store_upsert(project_id, store_id, Some(100)))
        .await
        .unwrap();
    let (store_root, manifest_path, database_path) =
        observation_store_paths(&profile_root, project_id);
    fs::create_dir_all(store_root.parent().unwrap()).unwrap();

    let outside_store = dir.path().join("outside-store");
    fs::create_dir_all(&outside_store).unwrap();
    fs::write(outside_store.join("store_manifest.json"), b"{}").unwrap();
    fs::write(outside_store.join("sessions.db"), b"").unwrap();
    symlink(&outside_store, &store_root).unwrap();
    let error = db
        .resolve_project_observation_store(&project_root)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ProjectObservationStoreError::NonCanonicalStore { .. }
    ));

    fs::remove_file(&store_root).unwrap();
    fs::create_dir_all(&store_root).unwrap();
    let outside_manifest = dir.path().join("outside-manifest.json");
    fs::write(&outside_manifest, b"{}").unwrap();
    symlink(&outside_manifest, &manifest_path).unwrap();
    fs::write(&database_path, b"").unwrap();
    let error = db
        .resolve_project_observation_store(&project_root)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ProjectObservationStoreError::NonCanonicalStore { .. }
    ));

    fs::remove_file(&manifest_path).unwrap();
    write_observation_store_manifest(&profile_root, &project_root, project_id);
    fs::remove_file(&database_path).unwrap();
    let outside_database = dir.path().join("outside-sessions.db");
    fs::write(&outside_database, b"").unwrap();
    symlink(&outside_database, &database_path).unwrap();
    let error = db
        .resolve_project_observation_store(&project_root)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ProjectObservationStoreError::NonCanonicalStore { .. }
    ));
    close_profile_runtime(db).await;
}

#[tokio::test]
async fn registry_remote_resolution_is_conservative_when_ambiguous() {
    let _guard = GLOBAL_REGISTRY_TEST_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
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

    assert!(
        db.resolve_unique_project_store_by_git_remote(
            "https://github.com/ScriptedAlchemy/tracedecay.git"
        )
        .await
        .is_none()
    );
    close_profile_runtime(db).await;
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
    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();
    close_profile_runtime(db).await;

    assert!(project_column_exists(&db_path, "path"));
    assert!(project_column_exists(&db_path, "tokens_saved"));

    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();
    db.upsert(&project_one, 11).await;
    db.upsert(&project_two, 22).await;
    db.upsert(&project_one.join("."), 33).await;

    assert_eq!(db.get_project_tokens(&project_one).await, 33);
    assert_eq!(db.get_project_tokens(&project_two.join(".")).await, 22);
    assert_eq!(db.global_tokens_saved().await, Some(55));
    assert_eq!(db.list_project_paths_compat().await.len(), 2);
    close_profile_runtime(db).await;
}

#[tokio::test]
async fn registry_gc_reaps_dead_paths_without_discarding_retained_store_authority() {
    let _guard = GLOBAL_REGISTRY_TEST_LOCK.lock().await;
    let profile = TempDir::new().unwrap();
    #[cfg(unix)]
    fs::set_permissions(profile.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let orphan_root = profile.path().join("gone-orphan");
    let retained_root = profile.path().join("gone-with-store");

    let db = HostAdmissionTestRuntimeV1::profile(profile.path())
        .await
        .unwrap();
    for root in [&orphan_root, &retained_root] {
        db.upsert(root, 1).await;
    }
    db.upsert_code_project("proj_orphan", &orphan_root, None, None, None)
        .await
        .unwrap();
    db.upsert_code_project("proj_retained", &retained_root, None, None, None)
        .await
        .unwrap();
    upsert_test_store(&db, "proj_retained", "store_retained").await;
    close_profile_runtime(db).await;

    let runtime =
        tracedecay::profile_registry_maintenance::ProfileRegistryMaintenanceRuntime::open(
            profile.path(),
        )
        .await
        .unwrap();
    let preview = runtime
        .registry_gc(profile.path(), None, false)
        .await
        .unwrap();
    assert_eq!(preview.code_project_candidate_count, 1);
    assert_eq!(preview.storage_project_candidate_count, 2);
    assert_eq!(preview.protected_code_project_count, 1);

    let applied = runtime
        .registry_gc(profile.path(), None, true)
        .await
        .unwrap();
    assert_eq!(applied.deleted_code_project_count, 1);
    assert_eq!(applied.deleted_storage_project_count, 2);
    drop(runtime);

    let db = HostAdmissionTestRuntimeV1::profile(profile.path())
        .await
        .unwrap();
    assert!(
        db.get_code_project("proj_orphan")
            .await
            .expect("registry read for a reaped orphan should not fault")
            .is_none()
    );
    assert!(
        db.get_code_project("proj_retained")
            .await
            .expect("registry read for a retained project should not fault")
            .is_some(),
        "a missing root must not discard authority for a retained store"
    );
    assert!(
        db.list_project_paths_compat().await.is_empty(),
        "dead savings-ledger paths should be reapable independently of stores"
    );
    close_profile_runtime(db).await;
}
