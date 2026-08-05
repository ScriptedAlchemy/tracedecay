//! Linked-worktree and same-remote clone identity tests.

use super::*;

#[tokio::test]
async fn linked_worktree_uses_initialized_git_common_dir_store_without_init() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let worktree = dir.path().join("repo-wt");
    let home = test_home(&dir);
    let profile_root = home.join(".tracedecay");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn main_only() {}\n").unwrap();
    let _home_guard = HomeGuard::set(&home);

    init_repo_with_commit(&project);

    let main = init_with_maintenance(&project).await.unwrap();
    let lifecycle = acquire_fixture_maintenance();
    let database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle,
        &profile_root,
        "seed linked worktree store",
    )
    .unwrap();
    main.index_all().await.unwrap();
    let main_store = main.store_layout().data_root.clone();
    let main_database = main.store_layout().graph_db_path.clone();
    main.close();
    drop(database_scope);
    drop(lifecycle);

    git(
        &project,
        &[
            "worktree",
            "add",
            "-b",
            "feature/worktree-auto",
            worktree.to_str().unwrap(),
        ],
    );
    fs::write(
        worktree.join("src/lib.rs"),
        "pub fn main_only() {}\npub fn worktree_only() {}\n",
    )
    .unwrap();

    assert_eq!(
        discover_project_root(&worktree.join("src")),
        None,
        "discovery must not walk from a linked worktree into the main checkout"
    );
    assert!(
        TraceDecay::has_initialized_store(&worktree).await,
        "linked worktree should resolve the already-initialized shared git store"
    );

    let worktree_cg = open_with_maintenance(&worktree).await.unwrap();
    assert_eq!(worktree_cg.project_root(), worktree.as_path());
    assert_eq!(worktree_cg.store_layout().data_root, main_store);
    assert_path_eq(
        &worktree_cg.store_layout().sessions_db_path,
        main_store.join("sessions.db"),
    );
    assert_path_eq(worktree_cg.db_path(), &main_database);
    assert!(
        !worktree.join(".tracedecay").exists(),
        "automatic worktree support must not require or create a per-worktree marker"
    );
    assert!(
        fs::read_dir(main_store.join("branches")).is_err(),
        "worktree/ref identity is snapshot provenance and must not create a branch database"
    );
}

#[tokio::test]
async fn detached_linked_worktree_uses_repository_identity_and_exact_route() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let worktree = dir.path().join("repo-detached");
    let home = test_home(&dir);
    let profile_root = home.join(".tracedecay");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn main_only() {}\n").unwrap();
    let _home_guard = HomeGuard::set(&home);

    init_repo_with_commit(&project);

    let main = init_with_maintenance(&project).await.unwrap();
    let lifecycle = acquire_fixture_maintenance();
    let database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle,
        &profile_root,
        "seed detached worktree store",
    )
    .unwrap();
    main.index_all().await.unwrap();
    let main_project_id = main
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("main checkout has a project identity");
    let main_store = main.store_layout().data_root.clone();
    let main_database = main.store_layout().graph_db_path.clone();
    main.close();
    drop(database_scope);
    drop(lifecycle);

    git(
        &project,
        &[
            "worktree",
            "add",
            "--detach",
            worktree.to_str().unwrap(),
            "HEAD",
        ],
    );
    fs::write(
        worktree.join("src/worktree_only.rs"),
        "pub fn detached_only() {}\n",
    )
    .unwrap();

    assert!(
        TraceDecay::has_initialized_store(&worktree).await,
        "detached worktree should resolve the repository's initialized store"
    );
    let detached = open_with_maintenance(&worktree).await.unwrap();
    assert_eq!(
        detached.store_layout().identity.project_id.as_deref(),
        Some(main_project_id.as_str())
    );
    assert_eq!(detached.store_layout().data_root, main_store);
    assert_eq!(detached.active_branch(), None);
    assert_eq!(detached.serving_branch(), None);
    assert_eq!(detached.project_root(), worktree);
    assert_path_eq(detached.db_path(), main_database);
}

#[tokio::test]
async fn linked_worktree_ignores_local_enrollment_that_would_shard_shared_store() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let worktree = dir.path().join("repo-wt");
    let home = test_home(&dir);
    let profile_root = home.join(".tracedecay");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn main_only() {}\n").unwrap();
    let _home_guard = HomeGuard::set(&home);
    init_repo_with_commit(&project);

    let main = init_with_maintenance(&project).await.unwrap();
    let main_project_id = main
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("primary checkout has a project identity");
    let main_store = main.store_layout().data_root.clone();
    main.close();

    git(
        &project,
        &[
            "worktree",
            "add",
            "-b",
            "feature/local-enrollment",
            worktree.to_str().unwrap(),
        ],
    );

    let stale_project_id = "proj_local_linked_worktree";
    let stale_data_root = profile_root.join(format!("projects/{stale_project_id}"));
    fs::create_dir_all(&stale_data_root).unwrap();
    write_enrollment_marker(
        &worktree,
        &EnrollmentMarker {
            project_id: stale_project_id.to_owned(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();

    let layout = TraceDecay::resolve_store_layout_for_identity_with_options(
        &worktree,
        &TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(profile_root.join("global.db")),
        },
    )
    .await
    .expect("linked worktree must resolve its repository's store");

    assert_eq!(
        layout.identity.project_id.as_deref(),
        Some(main_project_id.as_str())
    );
    assert_path_eq(&layout.data_root, &main_store);
    assert_path_eq(&layout.project_root, &worktree);
    assert_ne!(
        layout.identity.project_id.as_deref(),
        Some(stale_project_id),
        "a linked worktree marker must not create a second project store"
    );
}

#[tokio::test]
async fn registered_exact_root_ignores_sibling_worktree_manifests() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let first_worktree = dir.path().join("repo-wt-one");
    let second_worktree = dir.path().join("repo-wt-two");
    let home = test_home(&dir);
    let profile_root = home.join(".tracedecay");
    let global_db_path = profile_root.join("global.db");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn registered_root() {}\n").unwrap();
    let _home_guard = HomeGuard::set(&home);
    init_repo_with_commit(&project);

    let main = init_with_maintenance(&project).await.unwrap();
    let lifecycle = acquire_fixture_maintenance();
    let database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle,
        &profile_root,
        "seed registered worktree store",
    )
    .unwrap();
    main.index_all().await.unwrap();
    let main_project_id = main.store_layout().identity.project_id.clone().unwrap();
    let main_data_root = main.store_layout().data_root.clone();
    main.close();
    drop(database_scope);
    drop(lifecycle);

    for (branch, worktree) in [
        ("feature/registered-sibling-one", first_worktree.as_path()),
        ("feature/registered-sibling-two", second_worktree.as_path()),
    ] {
        git(
            &project,
            &["worktree", "add", "-b", branch, worktree.to_str().unwrap()],
        );
    }

    for (project_id, manifest_root) in [
        ("proj_registered_sibling_one", first_worktree.as_path()),
        ("proj_registered_sibling_two", second_worktree.as_path()),
    ] {
        let data_root = profile_root.join(format!("projects/{project_id}"));
        fs::create_dir_all(&data_root).unwrap();
        fs::write(data_root.join("tracedecay.db"), project_id).unwrap();
        fs::write(data_root.join("sessions.db"), b"sessions").unwrap();
        branch_meta::save_branch_meta(&data_root, &BranchMeta::new_for_dir(&data_root, "main"))
            .unwrap();
        write_store_manifest_to_path(
            &data_root.join(STORE_MANIFEST_FILENAME),
            &StoreManifest {
                schema_version: STORE_MANIFEST_SCHEMA_VERSION,
                project_id: Some(project_id.to_string()),
                store_kind: StoreKind::CodeProject,
                storage_mode: StorageMode::ProfileSharded,
                project_root: manifest_root.to_path_buf(),
                data_root,
                graph_db_relpath: "tracedecay.db".into(),
                sessions_db_relpath: "sessions.db".into(),
                branch_meta_relpath: "branch-meta.json".into(),
            },
        )
        .unwrap();
    }

    let git_common_dir = tracedecay::worktree::git_common_dir(&project).unwrap();
    let global_db = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .unwrap();
    let registered = global_db
        .resolve_project_store_by_identity(&project, Some(&git_common_dir))
        .await
        .expect("resolve exact main checkout")
        .expect("the exact main checkout must resolve through registered storage");
    assert_eq!(registered.project.project_id, main_project_id);

    fs::remove_file(repository_identity_path(&project).unwrap()).unwrap();
    let layout = TraceDecay::resolve_store_layout_for_identity_with_options(
        &project,
        &TraceDecayOpenOptions {
            profile_root: Some(profile_root),
            global_db_path: Some(global_db_path),
        },
    )
    .await
    .expect("a registered exact-root shard must ignore sibling worktree manifests");

    assert_eq!(
        layout.identity.project_id.as_deref(),
        Some(main_project_id.as_str())
    );
    assert_path_eq(&layout.data_root, &main_data_root);
}

#[tokio::test]
async fn same_remote_clone_is_not_considered_initialized_without_local_identity() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let remote = dir.path().join("remote.git");
    let project = dir.path().join("repo");
    let clone = dir.path().join("repo-clone");
    let home = test_home(&dir);
    let _home_guard = HomeGuard::set(&home);

    git(dir.path(), &["init", "--bare", remote.to_str().unwrap()]);
    git(
        dir.path(),
        &["clone", remote.to_str().unwrap(), project.to_str().unwrap()],
    );
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn main_only() {}\n").unwrap();
    git(&project, &["config", "user.email", "test@example.com"]);
    git(&project, &["config", "user.name", "TraceDecay Test"]);
    git(&project, &["add", "."]);
    git(&project, &["commit", "-m", "initial"]);
    git(&project, &["push", "origin", "HEAD:master"]);
    git(
        dir.path(),
        &["clone", remote.to_str().unwrap(), clone.to_str().unwrap()],
    );

    let registered_session_db = init_with_maintenance(&project)
        .await
        .unwrap()
        .store_layout()
        .sessions_db_path
        .clone();

    assert!(
        !TraceDecay::has_initialized_store(&clone).await,
        "a separate clone with the same origin is not a linked worktree and must not borrow the initialized store"
    );
    assert_ne!(
        resolve_project_session_db_path(&clone).unwrap(),
        registered_session_db,
        "session storage must not use a same-remote clone as repository identity",
    );

    let original_identity = tracedecay::worktree::git_common_dir(&project)
        .unwrap()
        .join("tracedecay-project.json");
    let copied_identity = tracedecay::worktree::git_common_dir(&clone)
        .unwrap()
        .join("tracedecay-project.json");
    fs::copy(original_identity, copied_identity).unwrap();
    let error = match open_with_maintenance(&clone).await {
        Ok(_) => panic!("a copied repository marker must not bind a second live clone"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("repository identity conflict"),
        "unexpected copied-marker error: {error}"
    );
}

#[tokio::test]
async fn renamed_checkout_session_db_follows_registered_store() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let remote = dir.path().join("remote.git");
    let original = dir.path().join("repo");
    let renamed = dir.path().join("repo-renamed");
    let home = test_home(&dir);
    let _home_guard = HomeGuard::set(&home);

    git(dir.path(), &["init", "--bare", remote.to_str().unwrap()]);
    git(
        dir.path(),
        &[
            "clone",
            remote.to_str().unwrap(),
            original.to_str().unwrap(),
        ],
    );
    fs::create_dir_all(original.join("src")).unwrap();
    fs::write(original.join("src/lib.rs"), "pub fn main_only() {}\n").unwrap();
    git(&original, &["config", "user.email", "test@example.com"]);
    git(&original, &["config", "user.name", "TraceDecay Test"]);
    git(&original, &["add", "."]);
    git(&original, &["commit", "-m", "initial"]);
    git(&original, &["push", "origin", "HEAD:master"]);

    let cg = init_with_maintenance(&original).await.unwrap();
    let registered_session_db = cg.store_layout().sessions_db_path.clone();
    drop(cg);

    // Move the whole checkout on disk; both its canonical root and git common
    // dir change, so registry identity resolution can no longer match by path.
    fs::rename(&original, &renamed).unwrap();
    git(&renamed, &["remote", "remove", "origin"]);

    let resolved = open_with_maintenance(&renamed)
        .await
        .expect("renamed checkout should resolve a registered store")
        .store_layout()
        .sessions_db_path
        .clone();
    assert_path_eq(&resolved, &registered_session_db);
    assert_path_eq(
        resolve_project_session_db_path(&renamed).unwrap(),
        &registered_session_db,
    );

    #[cfg(unix)]
    {
        let alias = dir.path().join("repo-alias");
        symlink(&renamed, &alias).unwrap();
        let via_alias = open_with_maintenance(&alias)
            .await
            .expect("symlink alias should retain repository identity")
            .store_layout()
            .sessions_db_path
            .clone();
        assert_path_eq(via_alias, registered_session_db);
    }
}

#[tokio::test]
async fn parent_index_excludes_nested_linked_worktree_sources() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let nested_worktree = project.join(".worktrees/feature");
    let home = test_home(&dir);
    let profile_root = home.join(".tracedecay");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn parent_only() {}\n").unwrap();
    let _home_guard = HomeGuard::set(&home);
    init_repo_with_commit(&project);

    git(
        &project,
        &[
            "worktree",
            "add",
            "-b",
            "feature/nested-index",
            nested_worktree.to_str().unwrap(),
        ],
    );
    fs::write(
        nested_worktree.join("src/lib.rs"),
        "pub fn parent_only() {}\npub fn nested_worktree_only() {}\n",
    )
    .unwrap();

    let mut parent = init_with_maintenance(&project).await.unwrap();
    let lifecycle = acquire_fixture_maintenance();
    let _database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle,
        &profile_root,
        "index parent worktree fixture",
    )
    .unwrap();
    parent.add_include_folders(&[".worktrees".to_string()]);
    parent.index_all().await.unwrap();

    assert!(
        !parent.search("parent_only", 10).await.unwrap().is_empty(),
        "the parent checkout must remain indexed"
    );
    assert!(
        parent
            .search("nested_worktree_only", 10)
            .await
            .unwrap()
            .is_empty(),
        "a nested linked worktree must be a separate project view, not duplicate parent source"
    );
}

#[tokio::test]
async fn same_remote_clone_session_db_does_not_borrow_registered_store() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let remote = dir.path().join("remote.git");
    let project = dir.path().join("repo");
    let clone = dir.path().join("repo-clone");
    let home = test_home(&dir);
    let _home_guard = HomeGuard::set(&home);

    git(dir.path(), &["init", "--bare", remote.to_str().unwrap()]);
    git(
        dir.path(),
        &["clone", remote.to_str().unwrap(), project.to_str().unwrap()],
    );
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn main_only() {}\n").unwrap();
    git(&project, &["config", "user.email", "test@example.com"]);
    git(&project, &["config", "user.name", "TraceDecay Test"]);
    git(&project, &["add", "."]);
    git(&project, &["commit", "-m", "initial"]);
    git(&project, &["push", "origin", "HEAD:master"]);
    git(
        dir.path(),
        &["clone", remote.to_str().unwrap(), clone.to_str().unwrap()],
    );

    let cg = init_with_maintenance(&project).await.unwrap();
    let registered_session_db = cg.store_layout().sessions_db_path.clone();
    drop(cg);

    // The original checkout still exists on disk, so the same-remote clone must
    // not inherit its registered session store even though the remote is unique
    // in the registry.
    let resolved = resolve_project_session_db_path(&clone)
        .expect("clone should still resolve a default session DB path");
    assert_ne!(
        normalize_test_path(&resolved),
        normalize_test_path(&registered_session_db),
        "a separate same-remote clone must not borrow another checkout's session store",
    );
}

#[tokio::test]
async fn same_remote_repositories_keep_distinct_persistent_identities() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let remote = dir.path().join("remote.git");
    let one = dir.path().join("repo-one");
    let two = dir.path().join("repo-two");
    let renamed_one = dir.path().join("repo-one-renamed");
    let home = test_home(&dir);
    let _home_guard = HomeGuard::set(&home);

    git(dir.path(), &["init", "--bare", remote.to_str().unwrap()]);
    git(
        dir.path(),
        &["clone", remote.to_str().unwrap(), one.to_str().unwrap()],
    );
    fs::create_dir_all(one.join("src")).unwrap();
    fs::write(one.join("src/lib.rs"), "pub fn main_only() {}\n").unwrap();
    git(&one, &["config", "user.email", "test@example.com"]);
    git(&one, &["config", "user.name", "TraceDecay Test"]);
    git(&one, &["add", "."]);
    git(&one, &["commit", "-m", "initial"]);
    git(&one, &["push", "origin", "HEAD:master"]);
    git(
        dir.path(),
        &["clone", remote.to_str().unwrap(), two.to_str().unwrap()],
    );

    let one_session_db = init_with_maintenance(&one)
        .await
        .unwrap()
        .store_layout()
        .sessions_db_path
        .clone();
    init_with_maintenance(&two).await.unwrap();

    fs::rename(&one, &renamed_one).unwrap();

    let resolved = open_with_maintenance(&renamed_one)
        .await
        .expect("moved checkout should resolve its persistent repository identity")
        .store_layout()
        .sessions_db_path
        .clone();
    assert_path_eq(&resolved, &one_session_db);
    assert_path_eq(
        resolve_project_session_db_path(&renamed_one).unwrap(),
        one_session_db,
    );
}

#[tokio::test]
async fn nested_linked_worktree_does_not_discover_parent_checkout_marker() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let worktree = project.join(".worktrees/feature-nested");
    let home = test_home(&dir);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn main_only() {}\n").unwrap();
    let _home_guard = HomeGuard::set(&home);

    init_repo_with_commit(&project);
    init_with_maintenance(&project).await.unwrap();

    git(
        &project,
        &[
            "worktree",
            "add",
            "-b",
            "feature/nested",
            worktree.to_str().unwrap(),
        ],
    );

    assert_eq!(
        discover_project_root(&worktree.join("src")),
        None,
        "a linked worktree inside the main checkout must not inherit the parent marker"
    );
    assert!(
        TraceDecay::has_initialized_store(&worktree).await,
        "nested linked worktree should still find the shared git store"
    );
}
