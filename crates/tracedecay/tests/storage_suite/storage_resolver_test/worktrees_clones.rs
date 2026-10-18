//! Linked-worktree and same-remote clone identity tests.

use super::*;

#[tokio::test]
async fn linked_worktree_uses_initialized_git_common_dir_store_without_init() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let worktree = dir.path().join("repo-wt");
    let home = test_home(&dir);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn main_only() {}\n").unwrap();
    let profile = test_profile(&home);

    init_repo_with_commit(&project);

    let main = init_with_maintenance(&profile, &project).await.unwrap();
    let main_store = main.store_layout().data_root.clone();
    let main_database = main.store_layout().graph_db_path.clone();
    main.close();

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
        profile.discover_project_root(&worktree.join("src")),
        Some(worktree.clone()),
        "discovery resolves the worktree's own root through the shared \
         repository identity, never the main checkout's path"
    );
    assert!(
        TraceDecay::has_initialized_store_with_options(
            &worktree,
            &TraceDecayOpenOptions::for_profile(&profile)
        )
        .await,
        "linked worktree should resolve the already-initialized shared git store"
    );

    let worktree_cg = open_with_maintenance(&profile, &worktree).await.unwrap();
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
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let worktree = dir.path().join("repo-detached");
    let home = test_home(&dir);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn main_only() {}\n").unwrap();
    let profile = test_profile(&home);

    init_repo_with_commit(&project);

    let main = init_with_maintenance(&profile, &project).await.unwrap();
    let main_project_id = main
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("main checkout has a project identity");
    let main_store = main.store_layout().data_root.clone();
    let main_database = main.store_layout().graph_db_path.clone();
    main.close();

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
        TraceDecay::has_initialized_store_with_options(
            &worktree,
            &TraceDecayOpenOptions::for_profile(&profile)
        )
        .await,
        "detached worktree should resolve the repository's initialized store"
    );
    let detached = open_with_maintenance(&profile, &worktree).await.unwrap();
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
async fn linked_worktree_refuses_retired_local_enrollment_until_reset() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let worktree = dir.path().join("repo-wt");
    let home = test_home(&dir);
    let profile_root = home.join(".tracedecay");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn main_only() {}\n").unwrap();
    let profile = test_profile(&home);
    init_repo_with_commit(&project);

    let main = init_with_maintenance(&profile, &project).await.unwrap();
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

    // A leftover from before the working-tree cutover: a retired enrollment
    // file inside the linked worktree. It is refused, never honored.
    let stale_project_id = "proj_local_linked_worktree";
    let stale_data_root = profile_root.join(format!("projects/{stale_project_id}"));
    fs::create_dir_all(&stale_data_root).unwrap();
    fs::create_dir_all(worktree.join(".tracedecay")).unwrap();
    fs::write(
        worktree.join(".tracedecay/enrollment.json"),
        format!("{{\"project_id\":\"{stale_project_id}\",\"storage_mode\":\"profile_sharded\"}}"),
    )
    .unwrap();
    let options = TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: Some(profile_root.join("global.db")),
    };

    let refused = TraceDecay::resolve_store_layout_for_identity_with_options(&worktree, &options)
        .await
        .expect_err("a retired worktree-local layout is a typed reset");
    assert_eq!(
        refused
            .reset_required_context()
            .map(|(authority, _)| authority),
        Some("project store"),
        "{refused:?}"
    );

    let retired =
        tracedecay_runtime_core::storage::retired_checkout_layout_dir(&profile_root, &worktree)
            .expect("the refused layout is the one the reset deletes");
    fs::remove_dir_all(retired).unwrap();
    let layout = TraceDecay::resolve_store_layout_for_identity_with_options(&worktree, &options)
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
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let first_worktree = dir.path().join("repo-wt-one");
    let second_worktree = dir.path().join("repo-wt-two");
    let home = test_home(&dir);
    let profile_root = home.join(".tracedecay");
    let global_db_path = profile_root.join("global.db");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn registered_root() {}\n").unwrap();
    let profile = test_profile(&home);
    init_repo_with_commit(&project);

    let main = init_with_maintenance(&profile, &project).await.unwrap();
    let main_project_id = main.store_layout().identity.project_id.clone().unwrap();
    let main_data_root = main.store_layout().data_root.clone();
    main.close();

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
        branch_meta::save_branch_meta(&data_root, &BranchMeta::new("main")).unwrap();
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
                sessions_schema_digest: None,
            },
        )
        .unwrap();
    }

    let git_common_dir = tracedecay_runtime_core::worktree::git_common_dir(&project).unwrap();
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

/// A bare `remote.git`, a `repo` clone that pushed one commit to it, and a
/// second full clone `repo-clone`, under `dir`.
fn same_remote_clones(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let remote = dir.join("remote.git");
    let project = dir.join("repo");
    let clone = dir.join("repo-clone");
    git(dir, &["init", "--bare", remote.to_str().unwrap()]);
    git(
        dir,
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
        dir,
        &["clone", remote.to_str().unwrap(), clone.to_str().unwrap()],
    );
    (remote, project, clone)
}

/// The registered `canonical_root` of `project_id` and its alias keys.
async fn registry_rows(profile: &ProfileRoot, project_id: &str) -> (String, Vec<String>) {
    let db = HostAdmissionTestRuntimeV1::profile(profile.data_dir())
        .await
        .unwrap();
    let context = db
        .project_registry_context_by_id(project_id)
        .await
        .expect("project stays registered");
    let mut aliases = context
        .aliases
        .into_iter()
        .map(|alias| alias.alias_path)
        .collect::<Vec<_>>();
    aliases.sort();
    (context.project.canonical_root, aliases)
}

fn count_prefixed(aliases: &[String], prefix: &str) -> usize {
    aliases
        .iter()
        .filter(|alias| alias.starts_with(prefix))
        .count()
}

fn open_project_id(cg: &TraceDecay) -> String {
    cg.store_layout().identity.project_id.clone().unwrap()
}

#[tokio::test]
async fn second_clone_adopts_the_initialized_repository() {
    let dir = TempDir::new().unwrap();
    let home = test_home(&dir);
    let profile = test_profile(&home);
    let (_remote, project, clone) = same_remote_clones(&canonical_temp_path(dir.path()));

    let first = init_with_maintenance(&profile, &project).await.unwrap();
    let registered_session_db = first.store_layout().sessions_db_path.clone();
    let project_id = open_project_id(&first);
    first.close();

    // This resolver deliberately takes no registry, and the clone carries no
    // marker yet, so only a registry-backed open can adopt it.
    assert!(
        !TraceDecay::has_initialized_store_with_options(
            &clone,
            &TraceDecayOpenOptions::for_profile(&profile)
        )
        .await,
        "registry-free resolution has no anchor to adopt the clone through"
    );

    let second = open_with_maintenance(&profile, &clone)
        .await
        .expect("a second full clone opens the initialized repository without init");
    assert_eq!(open_project_id(&second), project_id);
    assert_path_eq(
        &second.store_layout().sessions_db_path,
        &registered_session_db,
    );
    second.close();

    let (canonical_root, aliases) = registry_rows(&profile, &project_id).await;
    assert_eq!(
        canonical_root,
        RegisteredGlobalDb::canonical_project_key(&project),
        "the first enrolled checkout stays the registered root"
    );
    for root in [&project, &clone] {
        assert!(
            aliases.contains(&RegisteredGlobalDb::canonical_project_key(root)),
            "{} has a path alias: {aliases:?}",
            root.display()
        );
    }
    assert_eq!(
        count_prefixed(&aliases, "git-root-commit:"),
        1,
        "{aliases:?}"
    );
    assert_eq!(
        read_repository_identity_marker(&clone)
            .unwrap()
            .expect("the clone's own marker")
            .project_id,
        project_id
    );

    let original_identity = tracedecay_runtime_core::worktree::git_common_dir(&project)
        .unwrap()
        .join("tracedecay-project.json");
    let copied_identity = tracedecay_runtime_core::worktree::git_common_dir(&clone)
        .unwrap()
        .join("tracedecay-project.json");
    fs::copy(original_identity, copied_identity).unwrap();
    let error = match open_with_maintenance(&profile, &clone).await {
        Ok(_) => panic!("a copied repository marker must not bind a second live clone"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("repository identity conflict"),
        "unexpected copied-marker error: {error}"
    );
}

#[tokio::test]
async fn second_clone_registration_keeps_the_first_canonical_root() {
    let dir = TempDir::new().unwrap();
    let home = test_home(&dir);
    let profile = test_profile(&home);
    let (_remote, project, clone) = same_remote_clones(&canonical_temp_path(dir.path()));
    let first = init_with_maintenance(&profile, &project).await.unwrap();
    let project_id = open_project_id(&first);
    first.close();
    let expected_root = RegisteredGlobalDb::canonical_project_key(&project);

    let mut alias_counts = Vec::new();
    for root in [&clone, &project, &clone] {
        let cg = open_with_maintenance(&profile, root).await.unwrap();
        assert_eq!(open_project_id(&cg), project_id);
        cg.close();
        let (canonical_root, aliases) = registry_rows(&profile, &project_id).await;
        assert_eq!(
            canonical_root,
            expected_root,
            "opening {} moved the registered root",
            root.display()
        );
        alias_counts.push(aliases.len());
    }
    assert_eq!(alias_counts[0], alias_counts[2], "{alias_counts:?}");
    assert_eq!(alias_counts[1], alias_counts[2], "{alias_counts:?}");
}

#[tokio::test]
async fn unrelated_histories_merge_adds_an_alias_and_keeps_the_id() {
    let dir = TempDir::new().unwrap();
    let home = test_home(&dir);
    let profile = test_profile(&home);
    let root = canonical_temp_path(dir.path());
    let (_remote, project, _clone) = same_remote_clones(&root);
    let first = init_with_maintenance(&profile, &project).await.unwrap();
    let project_id = open_project_id(&first);
    first.close();

    git(&project, &["checkout", "--orphan", "vendored"]);
    git(&project, &["rm", "-r", "--cached", "--quiet", "."]);
    fs::write(project.join("VENDORED.md"), "unrelated history\n").unwrap();
    git(&project, &["add", "VENDORED.md"]);
    git(&project, &["commit", "-m", "unrelated root"]);
    git(&project, &["checkout", "-f", "master"]);
    git(
        &project,
        &[
            "merge",
            "--allow-unrelated-histories",
            "-m",
            "merge vendored",
            "vendored",
        ],
    );

    let reopened = open_with_maintenance(&profile, &project).await.unwrap();
    assert_eq!(open_project_id(&reopened), project_id);
    reopened.close();
    // The registration cache of this process already holds `project`, so the
    // new root is recorded by the next checkout that registers.
    let merged_clone = root.join("repo-merged-clone");
    git(
        &root,
        &[
            "clone",
            project.to_str().unwrap(),
            merged_clone.to_str().unwrap(),
        ],
    );
    let cloned = open_with_maintenance(&profile, &merged_clone)
        .await
        .unwrap();
    assert_eq!(open_project_id(&cloned), project_id);
    cloned.close();

    let (_, aliases) = registry_rows(&profile, &project_id).await;
    assert_eq!(
        count_prefixed(&aliases, "git-root-commit:"),
        2,
        "{aliases:?}"
    );
}

#[tokio::test]
async fn shallow_clone_adopts_through_remote_url() {
    let dir = TempDir::new().unwrap();
    let home = test_home(&dir);
    let profile = test_profile(&home);
    let root = canonical_temp_path(dir.path());
    let (remote, project, _clone) = same_remote_clones(&root);
    let first = init_with_maintenance(&profile, &project).await.unwrap();
    let project_id = open_project_id(&first);
    first.close();

    let shallow = root.join("repo-shallow");
    git(
        &root,
        &[
            "clone",
            "--depth",
            "1",
            &format!("file://{}", remote.display()),
            shallow.to_str().unwrap(),
        ],
    );
    git(
        &shallow,
        &["remote", "set-url", "origin", remote.to_str().unwrap()],
    );

    let cg = open_with_maintenance(&profile, &shallow)
        .await
        .expect("a shallow clone of the same remote opens the initialized repository");
    assert_eq!(open_project_id(&cg), project_id);
    cg.close();
}

#[tokio::test]
async fn second_profile_enrolls_under_the_existing_marker_without_rewriting_it() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let profile = test_profile(&test_home(&dir));
    let other_home = dir.path().join("other-home");
    fs::create_dir_all(&other_home).unwrap();
    let other_profile = test_profile(&canonical_temp_path(&other_home));
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn main_only() {}\n").unwrap();
    init_repo_with_commit(&project);
    // Another profile enrolled the repository and wrote the shared marker.
    let foreign = init_with_maintenance(&other_profile, &project)
        .await
        .unwrap();
    let marker_id = open_project_id(&foreign);
    foreign.close();

    assert!(
        open_with_maintenance(&profile, &project).await.is_err(),
        "a marker naming a shard this profile lacks does not open a store"
    );
    let first = init_with_maintenance(&profile, &project)
        .await
        .expect("first touch in this profile enrolls the repository");
    assert_eq!(open_project_id(&first), marker_id);
    first.close();
    let reopened = open_with_maintenance(&profile, &project).await.unwrap();
    assert_eq!(open_project_id(&reopened), marker_id);
    reopened.close();
    assert_eq!(
        read_repository_identity_marker(&project)
            .unwrap()
            .expect("marker")
            .project_id,
        marker_id
    );
}

#[tokio::test]
async fn renamed_checkout_session_db_follows_registered_store() {
    let dir = TempDir::new().unwrap();
    let remote = dir.path().join("remote.git");
    let original = dir.path().join("repo");
    let renamed = dir.path().join("repo-renamed");
    let home = test_home(&dir);
    let profile = test_profile(&home);

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

    let cg = init_with_maintenance(&profile, &original).await.unwrap();
    let registered_session_db = cg.store_layout().sessions_db_path.clone();
    drop(cg);

    // Move the whole checkout on disk; both its canonical root and git common
    // dir change, so registry identity resolution can no longer match by path.
    fs::rename(&original, &renamed).unwrap();
    git(&renamed, &["remote", "remove", "origin"]);

    let resolved = open_with_maintenance(&profile, &renamed)
        .await
        .expect("renamed checkout should resolve a registered store")
        .store_layout()
        .sessions_db_path
        .clone();
    assert_path_eq(&resolved, &registered_session_db);
    assert_path_eq(
        resolve_layout(&renamed, profile.data_dir())
            .unwrap()
            .sessions_db_path,
        &registered_session_db,
    );

    #[cfg(unix)]
    {
        let alias = dir.path().join("repo-alias");
        symlink(&renamed, &alias).unwrap();
        let via_alias = open_with_maintenance(&profile, &alias)
            .await
            .expect("symlink alias should retain repository identity")
            .store_layout()
            .sessions_db_path
            .clone();
        assert_path_eq(via_alias, registered_session_db);
    }
}

#[tokio::test]
async fn same_remote_repositories_share_one_persistent_identity() {
    let dir = TempDir::new().unwrap();
    let remote = dir.path().join("remote.git");
    let one = dir.path().join("repo-one");
    let two = dir.path().join("repo-two");
    let renamed_one = dir.path().join("repo-one-renamed");
    let home = test_home(&dir);
    let profile = test_profile(&home);

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

    let one_session_db = init_with_maintenance(&profile, &one)
        .await
        .unwrap()
        .store_layout()
        .sessions_db_path
        .clone();
    assert_path_eq(
        &init_with_maintenance(&profile, &two)
            .await
            .unwrap()
            .store_layout()
            .sessions_db_path,
        &one_session_db,
    );

    fs::rename(&one, &renamed_one).unwrap();

    let resolved = open_with_maintenance(&profile, &renamed_one)
        .await
        .expect("moved checkout should resolve its persistent repository identity")
        .store_layout()
        .sessions_db_path
        .clone();
    assert_path_eq(&resolved, &one_session_db);
    assert_path_eq(
        resolve_layout(&renamed_one, profile.data_dir())
            .unwrap()
            .sessions_db_path,
        one_session_db,
    );
}

#[tokio::test]
async fn nested_linked_worktree_does_not_discover_parent_checkout_marker() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let worktree = project.join(".worktrees/feature-nested");
    let home = test_home(&dir);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn main_only() {}\n").unwrap();
    let profile = test_profile(&home);

    init_repo_with_commit(&project);
    init_with_maintenance(&profile, &project).await.unwrap();

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
        profile.discover_project_root(&worktree.join("src")),
        Some(worktree.clone()),
        "a nested linked worktree resolves its own root through the shared \
         repository identity, never the parent checkout's path"
    );
    assert!(
        TraceDecay::has_initialized_store_with_options(
            &worktree,
            &TraceDecayOpenOptions::for_profile(&profile)
        )
        .await,
        "nested linked worktree should still find the shared git store"
    );
}
