//! `projects forget` core: removing exactly one registered project's rows and
//! store directories while sibling projects keep theirs (issue #730).

use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;
use tokio::sync::Mutex;
use tracedecay::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay::profile_registry_maintenance::ProfileRegistryMaintenanceRuntime;
use tracedecay_global_db::{GraphScopeUpsert, StoreArtifactUpsert, StoreInstanceUpsert};

static PROJECTS_FORGET_TEST_LOCK: Mutex<()> = Mutex::const_new(());

async fn close_profile_runtime(db: HostAdmissionTestRuntimeV1) {
    db.checkpoint_profile_database_for_test().await;
    drop(db);
}

/// Registers one complete project identity: code project row, root alias,
/// store instance, graph scope, store artifact, token-ledger row, and the
/// on-disk store directory with representative bytes.
async fn register_project_fixture(
    db: &HostAdmissionTestRuntimeV1,
    profile_root: &Path,
    project_id: &str,
    project_root: &Path,
) -> PathBuf {
    fs::create_dir_all(project_root).unwrap();
    db.upsert(project_root, 42).await;
    let project = db
        .upsert_code_project(project_id, project_root, None, None, Some("main"))
        .await
        .unwrap();
    let store = db
        .upsert_store_instance(StoreInstanceUpsert {
            store_id: format!("store_{project_id}"),
            project_id: project.project_id.clone(),
            store_kind: "code_project".to_string(),
            storage_mode: "profile_sharded".to_string(),
            store_relpath: format!("projects/{project_id}"),
            manifest_relpath: Some(format!("projects/{project_id}/store_manifest.json")),
            last_verified_at: Some(100),
            last_write_at: Some(101),
        })
        .await
        .unwrap();
    db.upsert_graph_scope(GraphScopeUpsert {
        graph_scope_id: format!("scope_{project_id}_main"),
        project_id: project.project_id.clone(),
        store_id: store.store_id.clone(),
        branch_name: "main".to_string(),
        db_relpath: format!("projects/{project_id}/tracedecay.db"),
        parent_scope_id: None,
        last_synced_at: Some(102),
        writable: true,
    })
    .await
    .unwrap();
    db.upsert_store_artifact(StoreArtifactUpsert {
        store_id: store.store_id,
        artifact_kind: "graph_db".to_string(),
        relpath: format!("projects/{project_id}/tracedecay.db"),
        size_bytes: Some(4096),
        schema_version: None,
        updated_at: Some(103),
    })
    .await
    .unwrap();
    let store_dir = profile_root.join("projects").join(project_id);
    fs::create_dir_all(store_dir.join("branches")).unwrap();
    fs::write(store_dir.join("tracedecay.db"), b"graph bytes").unwrap();
    fs::write(store_dir.join("sessions.db"), b"session bytes").unwrap();
    store_dir
}

fn secured_profile() -> TempDir {
    let profile = TempDir::new().unwrap();
    #[cfg(unix)]
    fs::set_permissions(profile.path(), fs::Permissions::from_mode(0o700)).unwrap();
    profile
}

#[tokio::test]
async fn forget_project_removes_exactly_one_registered_project() {
    let _guard = PROJECTS_FORGET_TEST_LOCK.lock().await;
    let profile = secured_profile();
    let root_a = profile.path().join("repo-a");
    let root_b = profile.path().join("repo-b");

    let db = HostAdmissionTestRuntimeV1::profile(profile.path())
        .await
        .unwrap();
    let store_dir_a = register_project_fixture(&db, profile.path(), "proj_a", &root_a).await;
    let store_dir_b = register_project_fixture(&db, profile.path(), "proj_b", &root_b).await;
    close_profile_runtime(db).await;

    {
        let lifecycle = tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(
            profile.path(),
            "projects forget test",
        )
        .unwrap();
        let _scope = tracedecay_runtime_core::db::enter_maintenance_database_scope(
            &lifecycle,
            profile.path(),
            "projects forget test",
        )
        .unwrap();
        let runtime = ProfileRegistryMaintenanceRuntime::open(profile.path())
            .await
            .unwrap();
        // Path-shaped and id-shaped selectors must resolve the same identity.
        let by_path = runtime
            .resolve_registered_project(&root_a)
            .await
            .unwrap()
            .expect("root path resolves the registered project");
        assert_eq!(by_path.project.project_id, "proj_a");
        let context = runtime
            .resolve_registered_project(Path::new("proj_a"))
            .await
            .unwrap()
            .expect("project id resolves the registered project");

        let report = runtime
            .forget_project(profile.path(), &context, false)
            .await
            .unwrap();

        assert_eq!(report.project_id, "proj_a");
        assert_eq!(report.removed_store_dirs, vec![store_dir_a.clone()]);
        assert!(report.absent_store_dirs.is_empty());
        assert!(report.kept_store_dirs.is_empty());
        assert_eq!(report.rows.code_projects_deleted, 1);
        assert_eq!(report.rows.path_ledger_rows_deleted, 1);
        assert!(
            runtime
                .resolve_registered_project(Path::new("proj_a"))
                .await
                .unwrap()
                .is_none(),
            "the forgotten identity must not resolve any more"
        );
        let survivor = runtime
            .resolve_registered_project(Path::new("proj_b"))
            .await
            .unwrap()
            .expect("the sibling project must survive untouched");
        assert_eq!(survivor.stores.len(), 1, "sibling store instance survives");
        assert_eq!(survivor.aliases.len(), 1, "sibling alias survives");
    }

    assert!(!store_dir_a.exists(), "forgotten store bytes are removed");
    assert!(
        store_dir_b.join("tracedecay.db").exists(),
        "sibling store bytes must survive byte-identical"
    );
    assert_eq!(
        fs::read(store_dir_b.join("sessions.db")).unwrap(),
        b"session bytes"
    );

    let db = HostAdmissionTestRuntimeV1::profile(profile.path())
        .await
        .unwrap();
    assert!(db.get_code_project("proj_a").await.unwrap().is_none());
    assert!(db.get_code_project("proj_b").await.unwrap().is_some());
    // The ledger stores the resolved root, so the expectation must resolve
    // too: on macOS a tempdir is handed out as `/var/folders/...` while the
    // registry records `/private/var/folders/...` for the same directory.
    assert_eq!(
        db.project_ledger_paths_for_test().await.unwrap(),
        vec![root_b.canonicalize().unwrap()],
        "only the forgotten project's token-ledger row is retired"
    );
    close_profile_runtime(db).await;
}

#[tokio::test]
async fn forget_project_keep_store_retires_rows_but_preserves_bytes() {
    let _guard = PROJECTS_FORGET_TEST_LOCK.lock().await;
    let profile = secured_profile();
    let root = profile.path().join("repo-kept");

    let db = HostAdmissionTestRuntimeV1::profile(profile.path())
        .await
        .unwrap();
    let store_dir = register_project_fixture(&db, profile.path(), "proj_kept", &root).await;
    close_profile_runtime(db).await;

    {
        let lifecycle = tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(
            profile.path(),
            "projects forget test",
        )
        .unwrap();
        let _scope = tracedecay_runtime_core::db::enter_maintenance_database_scope(
            &lifecycle,
            profile.path(),
            "projects forget test",
        )
        .unwrap();
        let runtime = ProfileRegistryMaintenanceRuntime::open(profile.path())
            .await
            .unwrap();
        let context = runtime
            .resolve_registered_project(Path::new("proj_kept"))
            .await
            .unwrap()
            .expect("registered project resolves");

        let report = runtime
            .forget_project(profile.path(), &context, true)
            .await
            .unwrap();

        assert!(report.removed_store_dirs.is_empty());
        assert_eq!(report.kept_store_dirs, vec![store_dir.clone()]);
        assert_eq!(report.rows.code_projects_deleted, 1);
        assert!(
            runtime
                .resolve_registered_project(Path::new("proj_kept"))
                .await
                .unwrap()
                .is_none()
        );
    }

    assert_eq!(
        fs::read(store_dir.join("tracedecay.db")).unwrap(),
        b"graph bytes",
        "--keep-store must preserve the store bytes exactly"
    );
}

#[tokio::test]
async fn forget_refuses_malformed_store_relpath_without_any_mutation() {
    let _guard = PROJECTS_FORGET_TEST_LOCK.lock().await;
    let profile = secured_profile();
    let root = profile.path().join("repo-escape");

    let db = HostAdmissionTestRuntimeV1::profile(profile.path())
        .await
        .unwrap();
    fs::create_dir_all(&root).unwrap();
    db.upsert_code_project("proj_escape", &root, None, None, None)
        .await
        .unwrap();
    db.upsert_store_instance(StoreInstanceUpsert {
        store_id: "store_escape".to_string(),
        project_id: "proj_escape".to_string(),
        store_kind: "code_project".to_string(),
        storage_mode: "profile_sharded".to_string(),
        store_relpath: "projects/../../escape".to_string(),
        manifest_relpath: None,
        last_verified_at: Some(100),
        last_write_at: Some(101),
    })
    .await
    .unwrap();
    close_profile_runtime(db).await;

    {
        let lifecycle = tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(
            profile.path(),
            "projects forget test",
        )
        .unwrap();
        let _scope = tracedecay_runtime_core::db::enter_maintenance_database_scope(
            &lifecycle,
            profile.path(),
            "projects forget test",
        )
        .unwrap();
        let runtime = ProfileRegistryMaintenanceRuntime::open(profile.path())
            .await
            .unwrap();
        let context = runtime
            .resolve_registered_project(Path::new("proj_escape"))
            .await
            .unwrap()
            .expect("registered project resolves");

        let error = runtime
            .forget_project(profile.path(), &context, false)
            .await
            .expect_err("a store relpath escaping the profile must refuse the forget");

        assert!(
            error.to_string().contains("not a plain relative path"),
            "unexpected refusal: {error}"
        );
        assert!(
            runtime
                .resolve_registered_project(Path::new("proj_escape"))
                .await
                .unwrap()
                .is_some(),
            "a refused forget must leave the registry rows untouched"
        );
    }
}
