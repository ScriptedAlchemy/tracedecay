use std::path::Path;

use super::bootstrap::run_git;
use super::*;

async fn notify_workspace_open(
    server: &crate::mcp::McpServer,
    session_id: &str,
    workspace_root: &Path,
) {
    let notification = crate::mcp::JsonRpcRequest {
        jsonrpc: "2.0".to_owned(),
        id: None,
        method: "tracedecay/hookEvent".to_owned(),
        params: Some(serde_json::json!({
            "agent": "codex",
            "event": "workspaceOpen",
            "cwd": workspace_root,
            "route": {
                "session_id": session_id,
                "cwd": workspace_root,
                "worktree": workspace_root,
            }
        })),
    };
    assert!(server.handle_request(&notification).await.is_none());
}

async fn files_for_session(
    server: &crate::mcp::McpServer,
    session_id: &str,
) -> crate::mcp::JsonRpcResponse {
    let request = crate::mcp::JsonRpcRequest {
        jsonrpc: "2.0".to_owned(),
        id: Some(serde_json::json!(1)),
        method: "tools/call".to_owned(),
        params: Some(serde_json::json!({
            "name": "tracedecay_files",
            "arguments": {
                "session_id": session_id,
            },
        })),
    };
    server
        .handle_request(&request)
        .await
        .expect("files response")
}

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
            "-b",
            "linked-route",
            linked.to_str().expect("utf-8 linked path"),
        ],
    );
    std::fs::remove_file(linked.join("README.md")).expect("remove primary-only source");
    std::fs::write(
        linked.join("linked.rs"),
        "pub fn linked_snapshot_only() -> u8 { 2 }\n",
    )
    .expect("linked-only source");

    let client_identity = test_client_identity_for(profile_root.clone());
    let layout = initialize_test_project(&primary, &client_identity).await;
    save_scheduled_automation(&layout.dashboard_root, true).await;
    let stale_project_id = "proj_stale_linked_worktree";
    crate::storage::write_enrollment_marker(
        &linked,
        &crate::storage::EnrollmentMarker {
            project_id: stale_project_id.to_owned(),
            storage_mode: crate::storage::StorageMode::ProfileSharded,
        },
    )
    .expect("write stale linked-worktree marker");
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
    assert_eq!(
        primary_graph.store_layout().graph_db_path,
        linked_graph.store_layout().graph_db_path,
        "both exact worktree views must derive their database authority from the canonical layout locator"
    );
    assert!(
        !profile_root
            .join("projects")
            .join(stale_project_id)
            .exists(),
        "a stale worktree-local marker must never create or open a second project store"
    );
    let branch_store_exists = std::fs::read_dir(layout.data_root.join("branches"))
        .ok()
        .into_iter()
        .flatten()
        .filter_map(std::result::Result::ok)
        .any(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "db")
        });
    assert!(
        !branch_store_exists,
        "opening a linked worktree must not create a branch database"
    );

    let session_id = "session.linked-worktree-follow-up";
    notify_workspace_open(linked_server.as_ref(), session_id, &linked).await;
    let routed = files_for_session(primary_server.as_ref(), session_id).await;
    assert!(
        routed.error.is_none(),
        "a follow-up on another daemon server must retain the linked route: {routed:?}"
    );
    let routed_text = routed
        .result
        .as_ref()
        .and_then(|result| result["content"].as_array())
        .and_then(|content| content.first())
        .and_then(|item| item["text"].as_str())
        .unwrap_or_else(|| panic!("files response must contain text: {routed:?}"));
    assert!(routed_text.contains("linked.rs"), "{routed_text}");
    assert!(!routed_text.contains("README.md"), "{routed_text}");

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
    let linked_key = ProjectServerKey::from_open_project(&linked_graph, &linked_handshake).unwrap();
    assert_eq!(
        primary_key.owner, linked_key.owner,
        "runtime and automation owners must derive from the canonical StoreLayout locator"
    );
    assert!(super::super::scheduler::same_scheduler_owner(
        &primary_key,
        &linked_key
    ));
    engine
        .automation_configured_override
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let primary_automation = engine
        .reconcile_automation_scheduler_locked(
            primary_key.clone(),
            primary.clone(),
            primary_handshake.clone(),
        )
        .await;
    assert!(matches!(
        primary_automation,
        crate::dashboard::AutomationSchedulerReconcileOutcome::Started
            | crate::dashboard::AutomationSchedulerReconcileOutcome::RunningNotified
    ));
    let linked_automation = engine
        .reconcile_automation_scheduler_locked(
            linked_key.clone(),
            linked.clone(),
            linked_handshake.clone(),
        )
        .await;
    assert!(matches!(
        linked_automation,
        crate::dashboard::AutomationSchedulerReconcileOutcome::RunningNotified
            | crate::dashboard::AutomationSchedulerReconcileOutcome::Exiting
    ));
    assert_eq!(
        engine
            .store_administration
            .automation_schedulers()
            .lock()
            .await
            .len(),
        1,
        "linked worktrees must share one project-wide automation owner"
    );

    {
        let mut servers = engine.store_administration.project_servers().lock().await;
        assert!(servers.remove(&linked_key).is_some());
    }
    drop(linked_server);
    let reopened_linked_server = engine
        .project_server(&linked_handshake)
        .await
        .expect("linked worktree must reopen through the retained canonical runtime");
    let reopened_linked_graph = reopened_linked_server.cg().await;
    assert_eq!(
        reopened_linked_graph
            .db()
            .retained_runtime()
            .publication()
            .publication_id,
        primary_graph
            .db()
            .retained_runtime()
            .publication()
            .publication_id,
        "reopening an exact linked route must not publish a second database owner"
    );
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
        crate::dashboard::AutomationSchedulerReconcileOutcome::RunningNotified
            | crate::dashboard::AutomationSchedulerReconcileOutcome::Exiting
    ));
    assert!(
        crate::storage::read_enrollment_marker(&linked)
            .expect("read linked marker")
            .is_some_and(|marker| marker.project_id == stale_project_id),
        "routing must ignore, not rewrite, a stale worktree-local marker"
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), engine.shutdown_all())
        .await
        .expect("linked-worktree shutdown must remain bounded");
}
