//! Linked-worktree and same-remote clone identity tests (split from
//! `storage_resolver_test.rs`).

use super::*;

#[tokio::test]
async fn linked_worktree_uses_initialized_git_common_dir_store_without_init() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let worktree = dir.path().join("repo-wt");
    let home = test_home(&dir);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn main_only() {}\n").unwrap();
    let _home_guard = HomeGuard::set(&home);

    init_repo_with_commit(&project);

    let main = TraceDecay::init(&project).await.unwrap();
    main.index_all().await.unwrap();
    let main_store = main.store_layout().data_root.clone();
    drop(main);

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

    let worktree_cg = TraceDecay::open(&worktree).await.unwrap();
    assert_eq!(worktree_cg.project_root(), worktree.as_path());
    assert_eq!(worktree_cg.store_layout().data_root, main_store);
    assert_eq!(
        resolved_project_session_db_path(&worktree).await.unwrap(),
        worktree_cg.store_layout().sessions_db_path,
        "session storage should follow the shared git-common-dir store too"
    );
    assert!(
        !worktree_cg
            .search("worktree_only", 10)
            .await
            .unwrap()
            .is_empty(),
        "opening a linked worktree should auto-track and sync its branch DB"
    );
    assert!(
        !worktree.join(".tracedecay").exists(),
        "automatic worktree support must not require or create a per-worktree marker"
    );

    let meta = branch_meta::load_branch_meta(&main_store).unwrap();
    assert!(
        meta.is_tracked("feature/worktree-auto"),
        "linked worktree branch should be tracked in the shared store"
    );
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

    TraceDecay::init(&project).await.unwrap();

    assert!(
        !TraceDecay::has_initialized_store(&clone).await,
        "a separate clone with the same origin is not a linked worktree and must not borrow the initialized store"
    );
    assert_eq!(
        resolved_project_session_db_path(&clone).await.unwrap(),
        project_session_db_path(&clone),
        "session storage must not use a same-remote clone as repository identity"
    );

    let original_identity = tracedecay::worktree::git_common_dir(&project)
        .unwrap()
        .join("tracedecay-project.json");
    let copied_identity = tracedecay::worktree::git_common_dir(&clone)
        .unwrap()
        .join("tracedecay-project.json");
    fs::copy(original_identity, copied_identity).unwrap();
    let error = match TraceDecay::open(&clone).await {
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

    let cg = TraceDecay::init(&original).await.unwrap();
    let registered_session_db = cg.store_layout().sessions_db_path.clone();
    drop(cg);

    // Move the whole checkout on disk; both its canonical root and git common
    // dir change, so registry identity resolution can no longer match by path.
    fs::rename(&original, &renamed).unwrap();
    git(&renamed, &["remote", "remove", "origin"]);

    let resolved = resolved_project_session_db_path(&renamed)
        .await
        .expect("renamed checkout should resolve a session DB path");
    assert_path_eq(&resolved, &registered_session_db);
    assert_ne!(
        normalize_test_path(&resolved),
        normalize_test_path(&project_session_db_path(&renamed)),
        "renamed checkout must not fork a fresh default-path session DB",
    );

    #[cfg(unix)]
    {
        let alias = dir.path().join("repo-alias");
        symlink(&renamed, &alias).unwrap();
        let via_alias = resolved_project_session_db_path(&alias)
            .await
            .expect("symlink alias should retain repository identity");
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

    let mut parent = TraceDecay::init(&project).await.unwrap();
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

    let cg = TraceDecay::init(&project).await.unwrap();
    let registered_session_db = cg.store_layout().sessions_db_path.clone();
    drop(cg);

    // The original checkout still exists on disk, so the same-remote clone must
    // not inherit its registered session store even though the remote is unique
    // in the registry.
    let resolved = resolved_project_session_db_path(&clone)
        .await
        .expect("clone should still resolve a default session DB path");
    assert_ne!(
        normalize_test_path(&resolved),
        normalize_test_path(&registered_session_db),
        "a separate same-remote clone must not borrow another checkout's session store",
    );
    assert_path_eq(&resolved, project_session_db_path(&clone));
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

    let one_session_db = TraceDecay::init(&one)
        .await
        .unwrap()
        .store_layout()
        .sessions_db_path
        .clone();
    TraceDecay::init(&two).await.unwrap();

    fs::rename(&one, &renamed_one).unwrap();

    let resolved = resolved_project_session_db_path(&renamed_one)
        .await
        .expect("moved checkout should resolve its persistent repository identity");
    assert_path_eq(&resolved, one_session_db);
    assert_ne!(
        normalize_test_path(&resolved),
        normalize_test_path(&project_session_db_path(&renamed_one)),
        "remote ambiguity must not fork the moved repository into a new path-hash store"
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
    TraceDecay::init(&project).await.unwrap();

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
