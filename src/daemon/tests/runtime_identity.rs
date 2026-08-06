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
    assert_eq!(
        primary_graph
            .db()
            .retained_runtime()
            .publication()
            .publication_id,
        linked_graph
            .db()
            .retained_runtime()
            .publication()
            .publication_id,
        "linked worktrees must share one registry publication, not merely one facade slot"
    );
    assert!(
        matches!(
            primary_graph
                .db()
                .retained_runtime()
                .binding()
                .shard_id
                .scope,
            tracedecay_store::StoreShardScopeV1::Project { .. }
        ),
        "the mutable graph writer must be owned by project identity; worktree identity is snapshot provenance"
    );

    primary_graph
        .db()
        .execute_write_batch(
            "seed linked-worktree writer queue",
            "CREATE TABLE linked_writer_queue(value INTEGER NOT NULL);
             INSERT INTO linked_writer_queue(value) VALUES (0);",
        )
        .await
        .expect("seed writer queue");
    let held = primary_graph
        .db()
        .begin_write_transaction("hold linked-worktree writer")
        .await
        .expect("hold canonical writer");
    held.execute("UPDATE linked_writer_queue SET value = 1", ())
        .await
        .expect("update held transaction");

    let waiting_database = linked_graph.db().clone();
    let waiting = tokio::spawn(async move {
        let transaction = waiting_database
            .begin_write_transaction("cancel queued linked-worktree writer")
            .await?;
        drop(transaction);
        Ok::<(), crate::errors::TraceDecayError>(())
    });
    tokio::task::yield_now().await;
    assert!(
        !waiting.is_finished(),
        "the second linked-worktree write must queue on the canonical writer lane"
    );
    waiting.abort();
    match waiting.await {
        Err(error) => assert!(
            error.is_cancelled(),
            "queued write cancellation must be terminal"
        ),
        Ok(_) => panic!("queued write must be cancelled"),
    }
    drop(held);
    let recovered = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        linked_graph
            .db()
            .begin_write_transaction("write after linked-worktree cancellation"),
    )
    .await
    .expect("canonical writer lane must recover within the shutdown budget")
    .expect("writer after cancellation");
    recovered
        .commit()
        .await
        .expect("commit after queued cancellation");
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
    tokio::time::timeout(std::time::Duration::from_secs(5), engine.shutdown_all())
        .await
        .expect("linked-worktree shutdown must remain bounded");
}
