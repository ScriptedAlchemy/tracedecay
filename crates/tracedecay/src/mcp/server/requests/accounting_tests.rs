use super::*;
use crate::mcp::server::writer_test_support::{init_indexed_repo, registered_context};

#[tokio::test]
async fn sequential_dispatches_with_the_same_wire_id_account_distinct_completions() {
    let (cg, project, authority) = init_indexed_repo().await;
    let server =
        McpServer::new_with_registered_test_context(registered_context(cg, &authority), Vec::new())
            .await
            .unwrap();
    let mut connection = server.new_connection_route_state().unwrap();
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_owned(),
        id: Some(json!(1)),
        method: "tools/call".to_owned(),
        params: Some(json!({
            "name": "tracedecay_status",
            "arguments": {"admission_only": true},
        })),
    };
    for expected_calls in 1..=2 {
        let response = server
            .dispatch_envelope(
                McpDispatchRequest::raw(&request),
                false,
                &mut connection,
                tracedecay_runtime_core::cancellation::CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(response.error.is_none(), "{response:?}");
        assert_ne!(response.result.as_ref().unwrap()["isError"], json!(true));
        assert!(
            server
                .ledger_writes_settled_within(Duration::from_secs(10))
                .await
        );
        let history = server
            .accounting_db
            .as_ref()
            .unwrap()
            .sum_savings(Some(project.path().to_str().unwrap()), 0)
            .await
            .unwrap();
        assert_eq!(
            history.calls, expected_calls,
            "sequential wire-ID reuse names a new dispatch, not an accounting replay"
        );
    }
    drop(connection);
    server.shutdown().await;
}

async fn new_attribution(server: &Arc<McpServer>) -> TokenSavingsAttribution {
    let identity = mint_global_request_id(GlobalRequestSurface::McpTokenAccounting).unwrap();
    server
        .cg_snapshot()
        .await
        .prepare_token_savings(&identity)
        .await
        .unwrap()
}

async fn queue_completed_response(
    server: &Arc<McpServer>,
    attribution: &TokenSavingsAttribution,
    saved: u64,
) {
    let mut result = ToolResult::new(json!({"content": []}), Vec::new());
    record_token_accounting(
        &mut result,
        ToolTokenAccounting {
            raw_file_tokens: 20,
            response_tokens: 20 - saved,
        },
    );
    let response = server
        .complete_tool_call(
            json!(1),
            "tracedecay_search".to_owned(),
            json!({"query": "completed response"}),
            None,
            ToolCallInvoker::Host,
            &json!({}),
            DispatchedToolCall {
                cg: server.cg_snapshot().await,
                accounting_attribution: Some(attribution.clone()),
                selected_owner: None,
                selected_scope: None,
                outcome: Ok(result),
                elapsed_us: None,
            },
            server,
        )
        .await;
    assert!(response.error.is_none(), "{response:?}");
}

async fn complete_response(
    server: &Arc<McpServer>,
    attribution: &TokenSavingsAttribution,
    saved: u64,
) {
    queue_completed_response(server, attribution, saved).await;
    assert!(
        server
            .ledger_writes_settled_within(Duration::from_secs(10))
            .await
    );
}

#[tokio::test]
async fn pending_completion_keeps_its_reset_epoch_and_responses_do_not_wait_for_the_writer() {
    let (cg, project, authority) = init_indexed_repo().await;
    let server =
        McpServer::new_with_registered_test_context(registered_context(cg, &authority), Vec::new())
            .await
            .unwrap();
    let cg = server.cg_snapshot().await;
    let admitted_before_reset = new_attribution(&server).await;
    let held_writer = cg
        .db()
        .begin_write_transaction("hold pending response writes")
        .await
        .unwrap();
    let reset = cg.reset_local_counter();
    tokio::pin!(reset);
    // Poll reset into the canonical FIFO writer lane before response writers.
    // It cannot complete while this transaction holds the same lane.
    tokio::select! {
        biased;
        result = &mut reset => panic!("reset bypassed the held writer: {result:?}"),
        () = tokio::task::yield_now() => {}
    }

    let mut connection = server.new_connection_route_state().unwrap();
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_owned(),
        id: Some(json!(1)),
        method: "tools/call".to_owned(),
        params: Some(json!({
            "name": "tracedecay_status",
            "arguments": {"admission_only": true},
        })),
    };
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        server.dispatch_envelope(
            McpDispatchRequest::raw(&request),
            false,
            &mut connection,
            tracedecay_runtime_core::cancellation::CancellationToken::new(),
        ),
    )
    .await
    .expect("request epoch capture must not take the accounting writer")
    .unwrap();
    assert!(response.error.is_none(), "{response:?}");
    assert_ne!(response.result.as_ref().unwrap()["isError"], json!(true));
    tokio::time::timeout(
        Duration::from_secs(5),
        queue_completed_response(&server, &admitted_before_reset, 7),
    )
    .await
    .expect("a completed response cannot await its durable accounting write");
    assert!(
        server.ledger_writes_started.load(Ordering::SeqCst)
            > server.ledger_writes_finished.load(Ordering::SeqCst),
        "both responses returned with observed accounting writes still pending"
    );
    drop(connection);
    held_writer.commit().await.unwrap();
    reset.await.unwrap();
    // Disconnect/shutdown drains the still-observed writes. The older positive
    // response reaches the writer after reset and belongs only to lifetime.
    server.shutdown().await;
    assert_eq!(cg.get_tokens_saved().await.unwrap(), 7);
    assert_eq!(cg.get_local_counter().await.unwrap(), 0);

    let reopened = McpServer::new_with_context(registered_context(
        authority.reopen_project_graph(project.path()).await,
        &authority,
    ))
    .await;
    let admitted_after_reset = new_attribution(&reopened).await;
    complete_response(&reopened, &admitted_after_reset, 11).await;
    complete_response(&reopened, &admitted_before_reset, 7).await;
    assert_eq!(
        reopened
            .cg_snapshot()
            .await
            .get_tokens_saved()
            .await
            .unwrap(),
        18
    );
    assert_eq!(
        reopened
            .cg_snapshot()
            .await
            .get_local_counter()
            .await
            .unwrap(),
        11
    );
    let history = reopened
        .accounting_db
        .as_ref()
        .unwrap()
        .sum_savings(Some(project.path().to_str().unwrap()), 0)
        .await
        .unwrap();
    assert_eq!(history.saved_tokens, 18);
    assert_eq!(
        history.calls, 3,
        "the old response replay cannot append another history row"
    );
    reopened.shutdown().await;
}

#[tokio::test]
async fn failed_epoch_preparation_preserves_response_and_zero_credit_activity() {
    let (cg, project, authority) = init_indexed_repo().await;
    let server =
        McpServer::new_with_registered_test_context(registered_context(cg, &authority), Vec::new())
            .await
            .unwrap();
    let admitted = new_attribution(&server).await;
    complete_response(&server, &admitted, 7).await;
    let cg = server.cg_snapshot().await;
    cg.db()
        .set_metadata("token_savings_reset_epoch", &"9".repeat(1000))
        .await
        .unwrap();

    let mut connection = server.new_connection_route_state().unwrap();
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_owned(),
        id: Some(json!(1)),
        method: "tools/call".to_owned(),
        params: Some(json!({
            "name": "tracedecay_status",
            "arguments": {"admission_only": true},
        })),
    };
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        server.dispatch_envelope(
            McpDispatchRequest::raw(&request),
            false,
            &mut connection,
            tracedecay_runtime_core::cancellation::CancellationToken::new(),
        ),
    )
    .await
    .expect("optional accounting preparation cannot withhold the response")
    .unwrap();
    assert!(response.error.is_none(), "{response:?}");
    assert_ne!(response.result.as_ref().unwrap()["isError"], json!(true));
    assert!(
        server
            .ledger_writes_settled_within(Duration::from_secs(10))
            .await
    );
    assert_eq!(cg.get_tokens_saved().await.unwrap(), 7);
    assert_eq!(cg.get_local_counter().await.unwrap(), 7);
    let database = server.accounting_db.as_ref().unwrap();
    let history = database
        .sum_savings(Some(project.path().to_str().unwrap()), 0)
        .await
        .unwrap();
    assert_eq!(history.saved_tokens, 7);
    assert_eq!(
        history.calls, 1,
        "unattributed activity cannot append a savings observation"
    );
    {
        let monitor = tracedecay_session_memory::monitor_ring::MmapReader::open_at(
            server.owner_profile().unwrap().data_dir(),
        )
        .unwrap();
        assert_eq!(monitor.write_idx(), 2);
        let refused = monitor.entry(1).unwrap();
        assert_eq!(refused.tool_name, "tracedecay_status");
        assert_eq!(refused.delta, 0);
    }
    let snapshot = database.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT metadata_json FROM analytics_events
         WHERE event_kind = 'mcp_tool_call' AND tool_name = 'tracedecay_status'",
            (),
        )
        .await
        .unwrap();
    let metadata: Value = serde_json::from_str(
        &rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(metadata["tokens_saved"], json!(0));
    assert_eq!(metadata["savings_commit"], json!("refused"));
    assert!(metadata["before_tokens"].is_number());
    assert!(metadata["after_tokens"].is_number());
    assert!(rows.next().await.unwrap().is_none());
    drop(rows);
    drop(snapshot);
    drop(connection);
    server.shutdown().await;
}

#[tokio::test]
async fn completed_response_replay_survives_reset_reopen_and_projection_failure() {
    let (cg, project, authority) = init_indexed_repo().await;
    let project_root = project.path();
    let server = McpServer::new_with_context(registered_context(cg, &authority)).await;
    let accounting_db = server.accounting_db.as_ref().unwrap().clone();
    let identity = new_attribution(&server).await;

    // Canonical credit must survive a failed optional profile projection. The
    // exact replay later repairs that projection without another local delta.
    let transaction = accounting_db.begin_write_transaction().await.unwrap();
    transaction
        .execute(
            "CREATE TRIGGER reject_token_projection BEFORE INSERT ON projects
             BEGIN SELECT RAISE(ABORT, 'profile token projection failed'); END",
            (),
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    complete_response(&server, &identity, 7).await;
    assert_eq!(
        server.cg_snapshot().await.get_tokens_saved().await.unwrap(),
        7
    );
    assert_eq!(
        accounting_db
            .try_get_project_tokens(project_root)
            .await
            .unwrap(),
        0
    );
    let transaction = accounting_db.begin_write_transaction().await.unwrap();
    transaction
        .execute("DROP TRIGGER reject_token_projection", ())
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    server
        .cg_snapshot()
        .await
        .reset_local_counter()
        .await
        .unwrap();
    complete_response(&server, &identity, 7).await;
    assert_eq!(
        server.cg_snapshot().await.get_tokens_saved().await.unwrap(),
        7
    );
    assert_eq!(
        server
            .cg_snapshot()
            .await
            .get_local_counter()
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        accounting_db
            .try_get_project_tokens(project_root)
            .await
            .unwrap(),
        7
    );
    let history = accounting_db
        .sum_savings(Some(project_root.to_str().unwrap()), 0)
        .await
        .unwrap();
    assert_eq!(history.saved_tokens, 7);
    assert_eq!(
        history.calls, 1,
        "an exact replay cannot append another savings row"
    );

    let snapshot = accounting_db.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT metadata_json FROM analytics_events
         WHERE event_kind = 'mcp_tool_call' AND tool_name = 'tracedecay_search'
         ORDER BY id",
            (),
        )
        .await
        .unwrap();
    let first: Value = serde_json::from_str(
        &rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
    )
    .unwrap();
    let replay: Value = serde_json::from_str(
        &rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(first["tokens_saved"], json!(7));
    assert_eq!(replay["tokens_saved"], json!(0));
    assert_eq!(replay["savings_commit"], json!("replayed"));
    assert_eq!(replay["before_tokens"], json!(20));
    assert!(rows.next().await.unwrap().is_none());
    drop(rows);
    drop(snapshot);
    {
        let monitor = tracedecay_session_memory::monitor_ring::MmapReader::open_at(
            server.owner_profile().unwrap().data_dir(),
        )
        .unwrap();
        assert_eq!(monitor.write_idx(), 2);
        assert_eq!(monitor.entry(0).unwrap().delta, 7);
        assert_eq!(monitor.entry(1).unwrap().delta, 0);
    }

    let other_identity = new_attribution(&server).await;
    assert_ne!(
        identity, other_identity,
        "distinct admitted completions need distinct identities even when their wire ID is reused"
    );
    complete_response(&server, &other_identity, 7).await;
    assert_eq!(
        server.cg_snapshot().await.get_tokens_saved().await.unwrap(),
        14
    );
    assert_eq!(
        server
            .cg_snapshot()
            .await
            .get_local_counter()
            .await
            .unwrap(),
        7
    );
    server.shutdown().await;
    drop(server);

    let reopened = McpServer::new_with_context(registered_context(
        authority.reopen_project_graph(project_root).await,
        &authority,
    ))
    .await;
    // A retried background completion retains its original identity even if
    // its caller observes the commit only after reopening the project store.
    complete_response(&reopened, &identity, 7).await;
    assert_eq!(
        reopened
            .cg_snapshot()
            .await
            .get_tokens_saved()
            .await
            .unwrap(),
        14
    );
    assert_eq!(
        reopened
            .cg_snapshot()
            .await
            .get_local_counter()
            .await
            .unwrap(),
        7
    );
    let history = accounting_db
        .sum_savings(Some(project_root.to_str().unwrap()), 0)
        .await
        .unwrap();
    assert_eq!(history.saved_tokens, 14);
    assert_eq!(history.calls, 2);

    let restarted_identity = new_attribution(&reopened).await;
    assert_ne!(identity, restarted_identity);
    assert_ne!(other_identity, restarted_identity);
    complete_response(&reopened, &restarted_identity, 7).await;
    assert_eq!(
        reopened
            .cg_snapshot()
            .await
            .get_tokens_saved()
            .await
            .unwrap(),
        21
    );
    assert_eq!(
        reopened
            .cg_snapshot()
            .await
            .get_local_counter()
            .await
            .unwrap(),
        14
    );
    let zero_identity = new_attribution(&reopened).await;
    complete_response(&reopened, &zero_identity, 0).await;
    complete_response(&reopened, &zero_identity, 0).await;
    // The zero-saving response already owns this identity. A changed positive
    // estimate is refused, rather than turning a replay into a new credit.
    complete_response(&reopened, &zero_identity, 7).await;
    assert_eq!(
        reopened
            .cg_snapshot()
            .await
            .get_tokens_saved()
            .await
            .unwrap(),
        21
    );
    assert_eq!(
        reopened
            .cg_snapshot()
            .await
            .get_local_counter()
            .await
            .unwrap(),
        14
    );
    let history = accounting_db
        .sum_savings(Some(project_root.to_str().unwrap()), 0)
        .await
        .unwrap();
    assert_eq!(history.saved_tokens, 21);
    assert_eq!(
        history.calls, 4,
        "zero-saving response and its retries count once"
    );
    reopened.shutdown().await;
}
