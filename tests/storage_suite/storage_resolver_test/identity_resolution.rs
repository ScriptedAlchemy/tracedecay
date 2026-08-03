//! Observation-store identity resolution and legacy cutover/adoption tests.

use super::*;

#[tokio::test]
async fn observation_store_resolver_uses_repository_marker_after_checkout_move() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let moved = dir.path().join("repo-moved");
    let profile_root = dir.path().join("profile");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn marker_only() {}\n").unwrap();
    init_repo_with_commit(&project);
    let git_common_dir = tracedecay::worktree::git_common_dir(&project).unwrap();
    let project_id = "proj_marker_only";
    let db = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .unwrap();
    let (store_root, database_path) = register_observation_store(
        &db,
        &profile_root,
        project_id,
        &project,
        Some(&git_common_dir),
    )
    .await;
    write_repository_identity_marker(&project, project_id).unwrap();

    fs::rename(&project, &moved).unwrap();
    assert!(
        db.project_registry_context_by_alias(&moved)
            .await
            .unwrap()
            .is_none(),
        "the moved checkout must not already have a canonical path alias"
    );
    assert_ne!(
        tracedecay::worktree::git_common_dir(&moved).unwrap(),
        git_common_dir,
        "moving a primary checkout should also move its git common dir"
    );

    let resolution = db
        .resolve_project_observation_store(&moved)
        .await
        .expect("the durable repository marker should preserve project identity");
    assert_eq!(resolution.project().project_id, project_id);
    assert_eq!(resolution.store_root(), store_root.canonicalize().unwrap());
    assert_eq!(
        resolution.database_path(),
        database_path.canonicalize().unwrap()
    );
}

#[tokio::test]
async fn observation_store_resolver_rejects_conflicting_path_common_dir_and_marker_identities() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let common_dir_owner = dir.path().join("common-dir-owner");
    let marker_owner = dir.path().join("marker-owner");
    let profile_root = dir.path().join("profile");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::create_dir_all(&common_dir_owner).unwrap();
    fs::create_dir_all(&marker_owner).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn ambiguous() {}\n").unwrap();
    init_repo_with_commit(&project);
    let git_common_dir = tracedecay::worktree::git_common_dir(&project).unwrap();
    let db = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .unwrap();

    db.upsert_code_project("proj_path", &project, None, None, None)
        .await
        .unwrap();
    db.upsert_code_project(
        "proj_common_dir",
        &common_dir_owner,
        Some(&git_common_dir),
        None,
        None,
    )
    .await
    .unwrap();
    db.upsert_code_project("proj_marker", &marker_owner, None, None, None)
        .await
        .unwrap();
    write_repository_identity_marker(&project, "proj_marker").unwrap();

    let error = db
        .resolve_project_observation_store(&project)
        .await
        .unwrap_err();
    let (project_root, mut project_ids) = match error {
        ProjectObservationStoreError::AmbiguousProjectIdentity {
            project_root,
            project_ids,
        } => (project_root, project_ids),
        other => panic!("expected ambiguous project identity, got {other:?}"),
    };
    project_ids.sort();
    assert_eq!(project_root, project.canonicalize().unwrap());
    assert_eq!(
        project_ids,
        vec![
            "proj_common_dir".to_string(),
            "proj_marker".to_string(),
            "proj_path".to_string(),
        ]
    );
}

#[tokio::test]
async fn legacy_profile_store_upgrade_preserves_data_across_repo_identity_changes() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let remote = dir.path().join("remote.git");
    let project = dir.path().join("repo");
    let moved = dir.path().join("repo-moved");
    let linked = dir.path().join("repo-linked");
    let clone = dir.path().join("repo-clone");
    let home = test_home(&dir);
    let profile_root = home.join(".tracedecay");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn legacy_sentinel() {}\n").unwrap();
    let _home_guard = HomeGuard::set(&home);
    init_repo_with_commit(&project);
    git(dir.path(), &["init", "--bare", remote.to_str().unwrap()]);
    git(
        &project,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&project, &["push", "-u", "origin", "main"]);
    git(&remote, &["symbolic-ref", "HEAD", "refs/heads/main"]);

    let legacy_project_id = "proj_legacy_path_hash";
    let cg = init_enrolled_legacy_shard(&project, legacy_project_id).await;
    let lifecycle = acquire_fixture_maintenance();
    let database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle,
        &profile_root,
        "seed legacy profile store",
    )
    .unwrap();
    cg.index_all().await.unwrap();
    let main_fact_id = cg
        .add_fact(fact_request("legacy main fact sentinel"))
        .await
        .unwrap()
        .fact
        .unwrap()
        .fact_id;
    let current_root = cg.store_layout().data_root.clone();
    let current_project_id = cg.store_layout().identity.project_id.clone().unwrap();

    cg.checkpoint().await.unwrap();
    cg.close();
    drop(database_scope);
    drop(lifecycle);
    git(&project, &["checkout", "-b", "feature/legacy-sentinel"]);
    let branch = open_with_maintenance(&project).await.unwrap();
    let lifecycle = acquire_fixture_maintenance();
    let database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle,
        &profile_root,
        "seed legacy branch store",
    )
    .unwrap();
    let branch_fact_id = branch
        .add_fact(fact_request("legacy branch fact sentinel"))
        .await
        .unwrap()
        .fact
        .unwrap()
        .fact_id;
    branch.checkpoint().await.unwrap();
    branch.close();
    drop(database_scope);
    drop(lifecycle);
    git(&project, &["checkout", "main"]);

    let sessions = HostAdmissionTestRuntimeV1::project(
        &profile_root,
        &project,
        ProjectId::new(current_project_id.clone()).unwrap(),
    )
    .await
    .unwrap();
    assert!(
        sessions
            .upsert_session_for_test(
                tracedecay::application::host_admission::HostAdmissionScope::Project,
                &SessionRecord {
                    provider: "codex".to_string(),
                    session_id: "legacy-session-sentinel".to_string(),
                    project_key: current_project_id,
                    project_path: project.to_string_lossy().to_string(),
                    title: Some("legacy session sentinel".to_string()),
                    started_at: Some(1_800_000_001),
                    ended_at: Some(1_800_000_002),
                    transcript_path: None,
                    metadata_json: None,
                    parent_session_id: None,
                    is_subagent: false,
                    agent_id: None,
                    parent_tool_use_id: None,
                },
            )
            .await
            .unwrap()
    );
    drop(sessions);

    let automation_sentinel = current_root.join("automation/migration-sentinel.json");
    fs::create_dir_all(automation_sentinel.parent().unwrap()).unwrap();
    fs::write(&automation_sentinel, br#"{"preserved":true}"#).unwrap();

    // The shard was born under its legacy identity, so its memory, sessions and
    // automation state already live at the legacy root; demote it to an
    // unadopted legacy store the resolver must rediscover and upgrade in place.
    let legacy_root = current_root.clone();
    demote_shard_to_unadopted_legacy(&project, &profile_root);

    let adopted = open_with_maintenance(&project)
        .await
        .expect("upgrade must adopt the manifest-backed legacy store");
    assert_path_eq(&adopted.store_layout().data_root, &legacy_root);
    assert_eq!(
        adopted
            .get_fact(main_fact_id)
            .await
            .unwrap()
            .unwrap()
            .content,
        "legacy main fact sentinel"
    );
    assert_eq!(
        fs::read_to_string(legacy_root.join("automation/migration-sentinel.json")).unwrap(),
        r#"{"preserved":true}"#
    );
    let lifecycle = acquire_fixture_maintenance();
    let database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle,
        &profile_root,
        "checkpoint adopted legacy store",
    )
    .unwrap();
    adopted.checkpoint().await.unwrap();
    adopted.close();
    drop(database_scope);
    drop(lifecycle);

    let sessions = HostAdmissionTestRuntimeV1::project(
        &profile_root,
        &project,
        ProjectId::new(legacy_project_id).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        sessions
            .session_for_test(
                tracedecay::application::host_admission::HostAdmissionScope::Project,
                "codex",
                "legacy-session-sentinel",
            )
            .await
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("legacy session sentinel")
    );
    drop(sessions);

    let branch = open_branch_with_maintenance(&project, "feature/legacy-sentinel")
        .await
        .unwrap();
    assert_eq!(
        branch
            .get_fact(branch_fact_id)
            .await
            .unwrap()
            .unwrap()
            .content,
        "legacy branch fact sentinel"
    );
    branch.close();

    let marker = read_repository_identity_marker(&project)
        .unwrap()
        .expect("successful adoption must persist repository identity");
    assert_eq!(marker.project_id, legacy_project_id);

    fs::rename(&project, &moved).unwrap();
    let reopened = open_with_maintenance(&moved).await.unwrap();
    assert_path_eq(&reopened.store_layout().data_root, &legacy_root);
    reopened.close();

    #[cfg(unix)]
    {
        let alias = dir.path().join("repo-alias");
        symlink(&moved, &alias).unwrap();
        let via_alias = open_with_maintenance(&alias).await.unwrap();
        assert_path_eq(&via_alias.store_layout().data_root, &legacy_root);
        via_alias.close();
    }

    git(
        &moved,
        &[
            "worktree",
            "add",
            "-b",
            "feature/adopted-linked",
            linked.to_str().unwrap(),
        ],
    );
    let linked_graph = open_with_maintenance(&linked).await.unwrap();
    assert_path_eq(&linked_graph.store_layout().data_root, &legacy_root);
    linked_graph.close();

    git(
        dir.path(),
        &["clone", remote.to_str().unwrap(), clone.to_str().unwrap()],
    );
    assert!(
        !TraceDecay::has_initialized_store(&clone).await,
        "same-remote clones must not adopt another checkout's orphan manifest"
    );
    let clone_graph = init_with_maintenance(&clone).await.unwrap();
    assert_ne!(
        normalize_test_path(&clone_graph.store_layout().data_root),
        normalize_test_path(&legacy_root),
        "a separate clone must mint its own store identity"
    );
    clone_graph.close();
}

#[tokio::test]
async fn empty_cutover_store_is_atomically_replaced_by_healthy_legacy_store() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let home = test_home(&dir);
    let profile_root = home.join(".tracedecay");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn cutover() {}\n").unwrap();
    let _home_guard = HomeGuard::set(&home);
    init_repo_with_commit(&project);

    let legacy_project_id = "proj_healthy_legacy";
    let old = init_enrolled_legacy_shard(&project, legacy_project_id).await;
    let lifecycle = acquire_fixture_maintenance();
    let database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle,
        &profile_root,
        "seed healthy legacy store",
    )
    .unwrap();
    let fact_id = old
        .add_fact(fact_request("healthy legacy cutover fact"))
        .await
        .unwrap()
        .fact
        .unwrap()
        .fact_id;
    let legacy_root = old.store_layout().data_root.clone();
    old.checkpoint().await.unwrap();
    old.close();
    drop(database_scope);
    drop(lifecycle);
    demote_shard_to_unadopted_legacy(&project, &profile_root);

    let cutover = default_profile_sharded_layout(&project, &profile_root).unwrap();
    let cutover_project_id = cutover.identity.project_id.clone().unwrap();
    initialize_empty_profile_layout(&cutover).await;
    write_repository_identity_marker(&project, &cutover_project_id).unwrap();

    let repaired = open_with_maintenance(&project)
        .await
        .expect("an empty cutover shard may safely yield to the healthy legacy shard");
    assert_path_eq(&repaired.store_layout().data_root, &legacy_root);
    assert_eq!(
        repaired.get_fact(fact_id).await.unwrap().unwrap().content,
        "healthy legacy cutover fact"
    );
    assert_eq!(
        read_repository_identity_marker(&project)
            .unwrap()
            .unwrap()
            .project_id,
        legacy_project_id
    );
    assert!(
        cutover.graph_db_path.is_file(),
        "empty shard stays as a backup"
    );
    assert!(
        cutover
            .data_root
            .join("store_manifest.identity-cutover-backup.json")
            .is_file(),
        "the retired empty shard must remain discoverable as an explicit backup"
    );
    assert!(!cutover.data_root.join(STORE_MANIFEST_FILENAME).exists());
    repaired.close();
}

#[tokio::test]
async fn empty_cutover_store_adopts_healthy_legacy_linked_worktree_store() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let linked = dir.path().join("repo-linked");
    let home = test_home(&dir);
    let profile_root = home.join(".tracedecay");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn linked_cutover() {}\n").unwrap();
    let _home_guard = HomeGuard::set(&home);
    init_repo_with_commit(&project);
    git(
        &project,
        &[
            "worktree",
            "add",
            "-b",
            "feature/legacy-linked-cutover",
            linked.to_str().unwrap(),
        ],
    );

    let legacy_project_id = "proj_healthy_linked_legacy";
    let old = init_enrolled_legacy_shard(&project, legacy_project_id).await;
    let lifecycle = acquire_fixture_maintenance();
    let database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle,
        &profile_root,
        "seed healthy linked legacy store",
    )
    .unwrap();
    let fact_id = old
        .add_fact(fact_request("healthy linked legacy cutover fact"))
        .await
        .unwrap()
        .fact
        .unwrap()
        .fact_id;
    let legacy_root = old.store_layout().data_root.clone();
    old.checkpoint().await.unwrap();
    old.close();
    drop(database_scope);
    drop(lifecycle);
    demote_shard_to_unadopted_legacy(&project, &profile_root);
    // Bind the legacy shard's manifest to the linked worktree; it shares the
    // git common dir with the primary checkout, so the resolver must adopt it by
    // repository identity when opening from the primary.
    rebind_manifest_project_root(&legacy_root, &linked);

    let cutover = default_profile_sharded_layout(&project, &profile_root).unwrap();
    let cutover_project_id = cutover.identity.project_id.clone().unwrap();
    initialize_empty_profile_layout(&cutover).await;
    write_repository_identity_marker(&project, &cutover_project_id).unwrap();

    let repaired = open_with_maintenance(&project)
        .await
        .expect("a linked worktree manifest with the same git common dir must be adopted");
    assert_path_eq(&repaired.store_layout().data_root, &legacy_root);
    assert_eq!(
        repaired.get_fact(fact_id).await.unwrap().unwrap().content,
        "healthy linked legacy cutover fact"
    );
    repaired.close();
}

#[tokio::test]
async fn corrupt_nonempty_cutover_store_reports_both_shards_without_switching() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let home = test_home(&dir);
    let profile_root = home.join(".tracedecay");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn split() {}\n").unwrap();
    let _home_guard = HomeGuard::set(&home);
    init_repo_with_commit(&project);

    let old = init_with_maintenance(&project).await.unwrap();
    let lifecycle = acquire_fixture_maintenance();
    let database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle,
        &profile_root,
        "seed split identity store",
    )
    .unwrap();
    old.add_fact(fact_request("legacy split identity fact"))
        .await
        .unwrap();
    let original_root = old.store_layout().data_root.clone();
    old.checkpoint().await.unwrap();
    old.close();
    drop(database_scope);
    drop(lifecycle);
    fs::remove_file(repository_identity_path(&project).unwrap()).unwrap();
    remove_sqlite_family(&profile_root.join("global.db"));

    let legacy_project_id = "proj_split_legacy";
    let legacy_root = profile_root.join(format!("projects/{legacy_project_id}"));
    relocate_store_as_legacy(&original_root, &legacy_root, &project, legacy_project_id);

    let cutover = default_profile_sharded_layout(&project, &profile_root).unwrap();
    let cutover_project_id = cutover.identity.project_id.clone().unwrap();
    initialize_empty_profile_layout(&cutover).await;
    remove_sqlite_family(&cutover.graph_db_path);
    fs::write(&cutover.graph_db_path, b"not a sqlite database").unwrap();
    let sessions = HostAdmissionTestRuntimeV1::project(
        &profile_root,
        &project,
        ProjectId::new(cutover_project_id.clone()).unwrap(),
    )
    .await
    .unwrap();
    assert!(
        sessions
            .upsert_session_for_test(
                tracedecay::application::host_admission::HostAdmissionScope::Project,
                &SessionRecord {
                    provider: "codex".to_string(),
                    session_id: "new-cutover-session".to_string(),
                    project_key: cutover_project_id.clone(),
                    project_path: project.to_string_lossy().to_string(),
                    title: Some("new cutover session".to_string()),
                    started_at: Some(1_800_000_010),
                    ended_at: None,
                    transcript_path: None,
                    metadata_json: None,
                    parent_session_id: None,
                    is_subagent: false,
                    agent_id: None,
                    parent_tool_use_id: None,
                },
            )
            .await
            .unwrap()
    );
    drop(sessions);
    fs::remove_file(enrollment_marker_path(&project)).unwrap();
    write_repository_identity_marker(&project, &cutover_project_id).unwrap();

    let error = TraceDecay::resolve_store_layout_for_identity(&project)
        .await
        .expect_err("nonempty split stores require explicit consolidation");
    let message = error.to_string();
    assert!(message.contains("identity cutover conflict"), "{message}");
    assert!(message.contains(&cutover_project_id), "{message}");
    assert!(message.contains(legacy_project_id), "{message}");
    assert!(message.contains("graph_health=corrupt"), "{message}");
    assert!(message.contains("sessions=1"), "{message}");
    assert!(message.contains("facts=1"), "{message}");
    assert!(message.contains("no files changed"), "{message}");
    assert!(
        message.contains("choose one shard and retire the other"),
        "{message}"
    );
    assert_eq!(
        read_repository_identity_marker(&project)
            .unwrap()
            .unwrap()
            .project_id,
        cutover_project_id
    );
    assert!(cutover.data_root.join(STORE_MANIFEST_FILENAME).is_file());
    assert!(legacy_root.join(STORE_MANIFEST_FILENAME).is_file());
}

#[tokio::test]
async fn ambiguous_legacy_store_adoption_preserves_every_candidate() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let profile_root = dir.path().join("profile");
    let global_db_path = profile_root.join("global.db");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn conflict() {}\n").unwrap();
    init_repo_with_commit(&project);

    for project_id in ["proj_legacy_one", "proj_legacy_two"] {
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
                project_root: project.clone(),
                data_root,
                graph_db_relpath: "tracedecay.db".into(),
                sessions_db_relpath: "sessions.db".into(),
                branch_meta_relpath: "branch-meta.json".into(),
            },
        )
        .unwrap();
    }

    let error = TraceDecay::resolve_store_layout_for_identity_with_options(
        &project,
        &TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(global_db_path),
        },
    )
    .await
    .expect_err("ambiguous legacy manifests must not be selected implicitly");

    assert!(
        error
            .to_string()
            .contains("ambiguous legacy profile stores")
    );
    for project_id in ["proj_legacy_one", "proj_legacy_two"] {
        assert_eq!(
            fs::read_to_string(profile_root.join(format!("projects/{project_id}/tracedecay.db")))
                .unwrap(),
            project_id,
            "conflict handling must retain every candidate as a recoverable backup"
        );
    }
    assert!(!repository_identity_path(&project).unwrap().exists());
}
