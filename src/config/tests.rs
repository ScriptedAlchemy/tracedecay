use super::TraceDecayConfig;
use std::fs;
use std::process::Command;
use tempfile::TempDir;

#[tokio::test]
async fn discover_project_root_with_identity_resolves_global_only_store() {
    let _profile = super::PinnedUserDataDir::new();
    let profile_root = crate::storage::default_profile_root().unwrap();
    let gdb = crate::global_db::GlobalDb::open().await.unwrap();
    let project_dir = TempDir::new().unwrap();
    let project_root = project_dir.path().canonicalize().unwrap();

    let project_id = "proj_identity_only";
    gdb.upsert_code_project(project_id, &project_root, None, None, None)
        .await
        .unwrap();
    gdb.upsert_store_instance(crate::global_db::StoreInstanceUpsert {
        store_id: "store_identity_only".to_string(),
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

    let layout = crate::storage::profile_sharded_layout(
        &project_root,
        &profile_root,
        &crate::storage::EnrollmentMarker {
            project_id: project_id.to_string(),
            storage_mode: crate::storage::StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    fs::create_dir_all(layout.graph_db_path.parent().unwrap()).unwrap();
    fs::write(&layout.graph_db_path, b"").unwrap();

    let status = Command::new("git")
        .arg("init")
        .arg(&project_root)
        .status()
        .unwrap();
    assert!(status.success(), "git init failed");

    assert!(
        super::discover_project_root(&project_root).is_none(),
        "sync discover_project_root must not see a global-only store"
    );
    assert_eq!(
        super::discover_project_root_with_identity(&project_root).await,
        Some(project_root.clone()),
        "identity wrapper must resolve a global-only registered store"
    );

    let nested = project_root.join("crates/inner");
    fs::create_dir_all(&nested).unwrap();
    assert_eq!(
        super::discover_project_root_with_identity(&nested)
            .await
            .map(|path| path.canonicalize().unwrap()),
        Some(project_root.clone()),
        "identity wrapper must walk up from a nested cwd to the registered root"
    );

    let bare = TempDir::new().unwrap();
    let bare_root = bare.path().canonicalize().unwrap();
    assert!(
        super::discover_project_root_with_identity(&bare_root)
            .await
            .is_none(),
        "a directory with no store must not resolve"
    );
}

#[tokio::test]
async fn config_path_with_identity_uses_registered_store_without_enrollment() {
    let _profile = super::PinnedUserDataDir::new();
    let profile_root = crate::storage::default_profile_root().unwrap();
    let gdb = crate::global_db::GlobalDb::open().await.unwrap();
    let project_dir = TempDir::new().unwrap();
    let project_root = project_dir.path().canonicalize().unwrap();
    let status = Command::new("git")
        .arg("init")
        .arg(&project_root)
        .status()
        .unwrap();
    assert!(status.success(), "git init failed");

    let project_id = "proj_config_identity";
    let git_common_dir = crate::worktree::git_common_dir(&project_root);
    gdb.upsert_code_project(
        project_id,
        &project_root,
        git_common_dir.as_deref(),
        None,
        None,
    )
    .await
    .unwrap();
    gdb.upsert_store_instance(crate::global_db::StoreInstanceUpsert {
        store_id: "store_config_identity".to_string(),
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
    let identity_layout = crate::storage::profile_sharded_layout(
        &project_root,
        &profile_root,
        &crate::storage::EnrollmentMarker {
            project_id: project_id.to_string(),
            storage_mode: crate::storage::StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    super::save_config_to_path(
        &identity_layout.config_path,
        &TraceDecayConfig {
            root_dir: "identity-config".to_string(),
            ..TraceDecayConfig::default()
        },
    )
    .unwrap();

    assert_eq!(
        super::get_config_path_with_identity(&project_root).await,
        identity_layout.config_path
    );
    assert_eq!(
        super::load_config_with_identity(&project_root)
            .await
            .unwrap()
            .root_dir,
        "identity-config"
    );
}

#[tokio::test]
async fn discover_project_root_with_identity_does_not_bind_non_git_child_to_parent_store() {
    let _profile = super::PinnedUserDataDir::new();
    let profile_root = crate::storage::default_profile_root().unwrap();
    let gdb = crate::global_db::GlobalDb::open().await.unwrap();
    let parent_dir = TempDir::new().unwrap();
    let parent_root = parent_dir.path().canonicalize().unwrap();
    let project_id = "proj_parent_identity_only";
    gdb.upsert_code_project(project_id, &parent_root, None, None, None)
        .await
        .unwrap();
    gdb.upsert_store_instance(crate::global_db::StoreInstanceUpsert {
        store_id: "store_parent_identity_only".to_string(),
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
    let layout = crate::storage::profile_sharded_layout(
        &parent_root,
        &profile_root,
        &crate::storage::EnrollmentMarker {
            project_id: project_id.to_string(),
            storage_mode: crate::storage::StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    fs::create_dir_all(layout.graph_db_path.parent().unwrap()).unwrap();
    fs::write(&layout.graph_db_path, b"").unwrap();

    let child = parent_root.join("scratch/deep");
    fs::create_dir_all(&child).unwrap();
    assert_eq!(
        super::discover_project_root_with_identity(&child).await,
        None,
        "non-git scratch directories must not inherit initialized parent stores"
    );
}

#[tokio::test]
async fn discover_project_root_with_identity_preserves_sync_fast_path() {
    let _profile = super::PinnedUserDataDir::new();
    let project_dir = TempDir::new().unwrap();
    let project_root = project_dir.path().canonicalize().unwrap();
    let db_dir = super::get_tracedecay_dir(&project_root);
    fs::create_dir_all(&db_dir).unwrap();
    fs::write(super::get_project_db_path(&project_root), b"").unwrap();

    let sync = super::discover_project_root(&project_root);
    assert!(sync.is_some(), "sync resolver must see a repo-local db");
    assert_eq!(
        super::discover_project_root_with_identity(&project_root).await,
        sync,
        "identity wrapper fast path must equal the sync result"
    );
}
