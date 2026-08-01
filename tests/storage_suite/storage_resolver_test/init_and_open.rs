//! TraceDecay init/open profile-shard placement tests.

use super::*;

#[tokio::test]
async fn trace_decay_init_defaults_to_profile_shard_without_repo_marker() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let child = project.join("src");
    let home = test_home(&dir);
    let profile_root = home.join(".tracedecay");
    fs::create_dir_all(&child).unwrap();
    let _home_guard = HomeGuard::set(&home);
    let project_id = default_profile_project_id(&project);
    let shard_root = profile_root.join(format!("projects/{project_id}"));

    assert!(!TraceDecay::is_initialized(&project));

    let cg = init_with_maintenance(&project).await.unwrap();

    assert_eq!(cg.store_layout().storage_mode, StorageMode::ProfileSharded);
    assert_path_eq(&cg.store_layout().data_root, &shard_root);
    assert_path_eq(cg.db_path(), shard_root.join("tracedecay.db"));
    assert_eq!(discover_project_root(&child), Some(project.clone()));
    assert!(
        !project.join(".tracedecay/tracedecay.db").exists(),
        "profile-sharded init must not create a repo-local graph DB"
    );
    assert!(
        !shard_root.join("config.json").exists(),
        "configuration is persisted in the registered store"
    );
    assert!(shard_root.join(STORE_MANIFEST_FILENAME).exists());
}

#[tokio::test]
async fn trace_decay_init_registers_default_profile_shard_globally() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let home = test_home(&dir);
    let profile_root = home.join(".tracedecay");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn registered() {}\n").unwrap();
    let _home_guard = HomeGuard::set(&home);
    init_repo_with_commit(&project);
    let project_id = default_profile_project_id(&project);

    init_with_maintenance(&project).await.unwrap().close();
    let db = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .unwrap();
    let resolution = db.resolve_project_store_by_alias(&project).await.unwrap();

    assert_eq!(resolution.project.project_id, project_id);
    let identity_path = tracedecay::worktree::git_common_dir(&project)
        .unwrap()
        .join("tracedecay-project.json");
    let identity: Value = serde_json::from_slice(&fs::read(&identity_path).unwrap()).unwrap();
    assert_eq!(identity["schema_version"], 1);
    assert_eq!(identity["project_id"], project_id);

    fs::remove_file(&identity_path).unwrap();
    drop(db);
    open_with_maintenance(&project).await.unwrap();
    assert!(
        identity_path.is_file(),
        "opening a legacy registered checkout must migrate it to durable repository identity"
    );
}

#[tokio::test]
async fn trace_decay_open_uses_profile_shard_paths_from_enrollment_marker() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let home = test_home(&dir);
    let profile_root = home.join(".tracedecay");
    let shard_root = profile_root.join("projects/proj_123");
    fs::create_dir_all(project.join(".tracedecay")).unwrap();
    fs::create_dir_all(&shard_root).unwrap();
    git(&project, &["init", "-b", "main"]);
    let _home_guard = HomeGuard::set(&home);

    write_enrollment(&project);
    let repo_local_config = TraceDecayConfig {
        root_dir: "repo-local-marker-config".to_string(),
        ..TraceDecayConfig::default()
    };
    fs::write(
        project.join(".tracedecay/config.json"),
        serde_json::to_string_pretty(&repo_local_config).unwrap(),
    )
    .unwrap();
    let shard_config = TraceDecayConfig {
        root_dir: project.to_string_lossy().to_string(),
        ..TraceDecayConfig::default()
    };
    fs::write(
        shard_root.join("config.json"),
        serde_json::to_string_pretty(&shard_config).unwrap(),
    )
    .unwrap();
    crate::common::initialize_test_database(&shard_root.join("tracedecay.db"))
        .await
        .unwrap();
    let meta = BranchMeta::new_for_dir(&shard_root, "main");
    branch_meta::save_branch_meta(&shard_root, &meta).unwrap();

    let opened = open_with_maintenance(&project).await.unwrap();

    assert_path_eq(opened.db_path(), shard_root.join("tracedecay.db"));
    assert_eq!(opened.get_config().root_dir, project.to_string_lossy());
    assert_eq!(opened.serving_branch(), Some("main"));
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn tracked_branch_project_memory_resolves_the_shared_project_store() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let home = test_home(&dir);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn shared_memory() {}\n").unwrap();
    let _home_guard = HomeGuard::set(&home);
    init_repo_with_commit(&project);

    let initialized = init_with_maintenance(&project).await.unwrap();
    let data_root = initialized.store_layout().data_root.clone();
    let project_graph = initialized.store_layout().graph_db_path.clone();
    let profile_root = maintenance_profile_root();
    let lifecycle = acquire_fixture_maintenance();
    let _database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle,
        &profile_root,
        "checkpoint shared project memory fixture",
    )
    .unwrap();
    initialized
        .db()
        .truncate_wal_for_test_artifact()
        .await
        .unwrap();
    initialized.close();
    drop(_database_scope);
    drop(lifecycle);

    let branch_relative = "branches/feature.db";
    let branch_graph = data_root.join(branch_relative);
    fs::create_dir_all(branch_graph.parent().unwrap()).unwrap();
    fs::copy(&project_graph, &branch_graph).unwrap();
    let mut meta = branch_meta::load_branch_meta(&data_root).unwrap();
    meta.add_branch("feature", branch_relative, "main");
    branch_meta::save_branch_meta(&data_root, &meta).unwrap();

    let branch = open_branch_with_maintenance(&project, "feature")
        .await
        .unwrap();
    assert_path_eq(branch.db_path(), &branch_graph);
    let memory = branch.open_project_store_db_read_only().await.unwrap();
    assert_path_eq(memory.database_path(), &project_graph);
}
