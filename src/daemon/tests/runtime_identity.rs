use super::bootstrap::run_git;
use super::*;

#[cfg(unix)]
#[tokio::test]
async fn concurrent_same_identity_worktrees_keep_exact_server_and_scheduler_bindings() {
    let home = TempDir::new().expect("isolated home");
    let root = home.path().canonicalize().expect("canonical home");
    let primary = root.join("primary");
    let linked = root.join("linked");
    let profile_root = root.join("profile");
    std::fs::create_dir_all(&primary).expect("create primary repository");
    run_git(&primary, &["init", "-b", "main", "--quiet"]);
    std::fs::write(primary.join("README.md"), "shared authority\n").expect("fixture");
    run_git(&primary, &["add", "."]);
    run_git(&primary, &["commit", "-m", "fixture", "--quiet"]);
    run_git(
        &primary,
        &[
            "worktree",
            "add",
            "--force",
            linked.to_str().expect("utf-8 linked path"),
            "main",
        ],
    );

    let client_identity = test_client_identity_for(profile_root.clone());
    initialize_test_project(&primary, &client_identity).await;
    let _database_scope =
        enter_test_daemon_database_scope(&profile_root, "shared worktree authority");
    let engine = test_daemon_engine_for_profile(&profile_root);
    let primary_handshake = DaemonHandshake {
        project_path: Some(primary.clone()),
        client_identity: client_identity.clone(),
        ..test_handshake_defaults()
    };
    let linked_handshake = DaemonHandshake {
        project_path: Some(linked.clone()),
        client_identity,
        ..test_handshake_defaults()
    };

    let (primary_server, linked_server) = tokio::join!(
        engine.project_server(&primary_handshake),
        engine.project_server(&linked_handshake),
    );
    let primary_server = primary_server.expect("primary project must open");
    let linked_server = linked_server
        .expect("linked worktree must concurrently open through the primary authority");

    assert!(
        !Arc::ptr_eq(&primary_server, &linked_server),
        "each exact worktree route must retain its own project server"
    );
    let primary_graph = primary_server.cg().await;
    let linked_graph = linked_server.cg().await;
    assert!(
        !Arc::ptr_eq(&primary_graph, &linked_graph),
        "each project server must retain its exact worktree graph runtime"
    );
    assert_eq!(primary_graph.project_root(), primary);
    assert_eq!(linked_graph.project_root(), linked);
    assert_eq!(
        primary_graph.db_path(),
        linked_graph.db_path(),
        "linked worktrees must share one project store authority"
    );
    assert_eq!(
        primary_graph.db().retained_runtime().runtime_identity(),
        linked_graph.db().retained_runtime().runtime_identity(),
        "linked worktree facades must share one physical store runtime"
    );
    let primary_key =
        ProjectServerKey::from_open_project(&primary_graph, &primary_handshake).unwrap();
    {
        let mut servers = engine.store_administration.project_servers().lock().await;
        assert_eq!(servers.servers.len(), 2);
        assert_eq!(servers.aliases.len(), 2);
        assert!(servers.remove(&primary_key).is_some());
    }

    assert!(matches!(
        engine
            .reconcile_automation_scheduler_locked(
                primary_key.clone(),
                primary.clone(),
                primary_handshake.clone(),
            )
            .await,
        crate::dashboard::AutomationSchedulerReconcileOutcome::OwnerUnavailable
    ));
    assert!(matches!(
        engine
            .start_memory_repair_scheduler(primary_key, primary, primary_handshake)
            .await,
        super::super::memory_repair_scheduler::MemoryRepairSchedulerReconcileOutcome::LifecycleInactive
    ));
    assert!(
        crate::storage::read_enrollment_marker(&linked)
            .expect("read linked marker")
            .is_none(),
        "linked route must not acquire a second enrollment marker"
    );
    engine.shutdown_all().await;
}
