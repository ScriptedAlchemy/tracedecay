use crate::common::EnvVarGuard;
use crate::mcp_server_test::support::*;
use serde_json::json;

#[tokio::test]
async fn search_call_writes_savings_ledger_row() {
    let fixture = setup_accounted_server().await;
    let (server, server_handle) = (fixture.server.clone(), fixture.server.clone());
    let (proj_tmp, db_path) = (&fixture.project, &fixture.global_db_path);

    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(9001),
            "tools/call",
            json!({
                "name": "tracedecay_search",
                "arguments": { "query": "hello" }
            }),
        )],
    )
    .await;

    // Verify the request completed successfully.
    let resp_str = responses
        .iter()
        .find(|r| parse_response(r)["id"] == 9001)
        .expect("should have a response for id=9001");
    let resp = parse_response(resp_str);
    assert!(resp["error"].is_null(), "search should not error");

    settled_ledger_total(&server_handle, db_path, proj_tmp.path(), 1).await;
}

#[tokio::test]
async fn search_call_writes_mcp_runtime_analytics_event() {
    let fixture = setup_accounted_savings_server().await;
    let (server, server_handle) = (fixture.server.clone(), fixture.server.clone());
    let (proj_tmp, db_path) = (&fixture.project, &fixture.global_db_path);
    let project_path = proj_tmp
        .path()
        .canonicalize()
        .expect("project path canonicalizes")
        .to_string_lossy()
        .to_string();

    let resp = call_tool(
        server,
        9002,
        "tracedecay_search",
        json!({
            "query": "helper",
            "session_id": "mcp-session-9002"
        }),
    )
    .await;

    assert!(resp["error"].is_null(), "search should not error");
    let (before, after) = parse_metrics_line(&resp).expect("metrics line present");

    settled_ledger_total(&server_handle, db_path, proj_tmp.path(), 1).await;
    assert_eq!(
        mcp_runtime_event_count(db_path, "mcp-session-9002").await,
        1,
        "one MCP runtime analytics event should be recorded per tool call"
    );
    let event = expect_mcp_runtime_event(
        db_path,
        "tracedecay_search",
        "mcp-session-9002",
        "durable MCP runtime analytics event",
    )
    .await;
    let metadata = analytics_metadata(&event);

    assert_eq!(event.session_id.as_deref(), Some("mcp-session-9002"));
    assert_eq!(event.project_id, project_path);
    assert!(
        event
            .tool_category
            .as_deref()
            .is_some_and(|category| !category.is_empty()),
        "category should be taxonomy-backed"
    );
    assert_eq!(event.outcome.as_deref(), Some("success"));
    assert_eq!(metadata["before_tokens"], before);
    assert_eq!(metadata["after_tokens"], after);
    assert_eq!(metadata["tokens_saved"], before - after);
    assert_eq!(metadata["transport"], "mcp");
    assert_eq!(metadata["tool_kind"], "mcp_tool");
}

#[tokio::test]
async fn context_call_writes_memory_match_analytics_without_fact_bodies() {
    let fixture = setup_accounted_server().await;
    let (server, server_handle) = (fixture.server.clone(), fixture.server.clone());
    let db_path = &fixture.global_db_path;

    let fact_content =
        "Durable context memory analytics should report counts without leaking fact bodies.";
    let added = call_tool(
        server.clone(),
        9005,
        "tracedecay_fact_store",
        json!({
            "action": "add",
            "content": fact_content,
            "category": "decision",
            "entity": "context memory analytics",
            "trust": 0.94,
            "source": "mcp-server-memory-analytics-test"
        }),
    )
    .await;
    assert!(added["error"].is_null(), "fact add should not error");

    let resp = call_tool(
        server,
        9006,
        "tracedecay_context",
        json!({
            "task": "context memory analytics",
            "session_id": "mcp-session-9006"
        }),
    )
    .await;
    assert!(resp["error"].is_null(), "context should not error");
    assert!(
        resp["result"].get("_tracedecay_analytics").is_none(),
        "internal analytics metadata must not leak to clients"
    );
    assert!(
        !resp["result"]
            .to_string()
            .contains("context_memory_analytics")
            && !resp["result"].to_string().contains("_tracedecay_analytics"),
        "internal analytics metadata must not leak inside response content"
    );

    server_handle.ledger_writes_settled().await;
    let event = expect_mcp_runtime_event(
        db_path,
        "tracedecay_context",
        "mcp-session-9006",
        "durable context MCP runtime analytics event",
    )
    .await;
    let metadata = analytics_metadata(&event);

    assert_eq!(event.outcome.as_deref(), Some("success"));
    assert_eq!(metadata["context_memory"]["include_memory"], true);
    assert!(
        metadata["context_memory"]["match_count"]
            .as_u64()
            .is_some_and(|count| count >= 1),
        "context analytics should expose bounded memory match counts: {metadata}"
    );
    assert!(
        !metadata.to_string().contains(fact_content),
        "analytics metadata must not persist fact bodies"
    );
}

#[tokio::test]
async fn failed_tool_call_writes_mcp_runtime_analytics_event() {
    let fixture = setup_accounted_server().await;
    let (server, server_handle) = (fixture.server.clone(), fixture.server.clone());
    let db_path = &fixture.global_db_path;

    let resp = call_tool(
        server,
        9003,
        "tracedecay_not_a_real_tool",
        json!({ "session_id": "mcp-session-9003" }),
    )
    .await;

    assert!(resp["error"].is_object(), "unknown tool should error");

    server_handle.ledger_writes_settled().await;
    assert_eq!(
        mcp_runtime_event_count(db_path, "mcp-session-9003").await,
        1,
        "failed MCP tool calls should still record analytics"
    );
    let event = expect_mcp_runtime_event(
        db_path,
        "tracedecay_not_a_real_tool",
        "mcp-session-9003",
        "durable failed MCP runtime analytics event",
    )
    .await;
    let metadata = analytics_metadata(&event);

    assert_eq!(event.session_id.as_deref(), Some("mcp-session-9003"));
    assert_eq!(event.outcome.as_deref(), Some("error"));
    assert_eq!(metadata["before_tokens"], 0);
    assert_eq!(metadata["after_tokens"], 0);
    assert_eq!(metadata["tokens_saved"], 0);
    assert_eq!(metadata["transport"], "mcp");
    assert_eq!(metadata["tool_kind"], "mcp_tool");
    let failure_reason = metadata["failure_reason"]
        .as_str()
        .expect("failure_reason should be a string");
    assert!(
        failure_reason.contains("unknown tool") && failure_reason.contains("not_a_real_tool"),
        "failure_reason should carry the real dispatch error, not a generic marker, got: {failure_reason}"
    );
}

#[tokio::test]
async fn skill_view_call_writes_skill_arguments_to_mcp_runtime_analytics() {
    let fixture = setup_accounted_server().await;
    let (server, server_handle) = (fixture.server.clone(), fixture.server.clone());
    let db_path = &fixture.global_db_path;

    let resp = call_tool(
        server,
        9004,
        "tracedecay_skill_view",
        json!({
            "id": "repo-hygiene",
            "session_id": "mcp-session-9004"
        }),
    )
    .await;

    assert!(
        resp["error"].is_object(),
        "missing fixture skill should make the tool call fail"
    );

    server_handle.ledger_writes_settled().await;
    let event = expect_mcp_runtime_event(
        db_path,
        "tracedecay_skill_view",
        "mcp-session-9004",
        "durable skill-view MCP runtime analytics event",
    )
    .await;
    let metadata = analytics_metadata(&event);

    assert_eq!(event.outcome.as_deref(), Some("error"));
    assert_eq!(metadata["arguments"]["id"], "repo-hygiene");
    assert_eq!(metadata["function"]["name"], "tracedecay_skill_view");
    assert_eq!(metadata["function"]["arguments"]["id"], "repo-hygiene");
}

#[tokio::test]
async fn semantic_tool_failure_writes_error_mcp_runtime_analytics_event() {
    let fixture = setup_accounted_server().await;
    let (server, server_handle) = (fixture.server.clone(), fixture.server.clone());
    let db_path = &fixture.global_db_path;

    let resp = call_tool(
        server,
        9004,
        "tracedecay_changelog",
        json!({
            "from_ref": "definitely-not-a-ref",
            "to_ref": "also-not-a-ref",
            "_meta": { "sessionId": "mcp-session-9004" }
        }),
    )
    .await;

    assert!(resp["error"].is_null(), "semantic failures are MCP results");
    assert_eq!(resp["result"]["isError"], true);

    server_handle.ledger_writes_settled().await;
    let event = expect_mcp_runtime_event(
        db_path,
        "tracedecay_changelog",
        "mcp-session-9004",
        "durable semantic-failure MCP runtime analytics event",
    )
    .await;

    assert_eq!(event.session_id.as_deref(), Some("mcp-session-9004"));
    assert_eq!(event.outcome.as_deref(), Some("error"));
}

#[tokio::test]
async fn structural_edit_failure_writes_real_failure_reason_to_analytics() {
    let fixture = setup_accounted_server().await;
    let (server, server_handle) = (fixture.server.clone(), fixture.server.clone());
    let db_path = &fixture.global_db_path;

    // An anchor mismatch (str_replace's `old_str` not present in the file) is
    // a structural failure the handler already knows about via
    // `EditResult::success == false` — the resulting analytics event should
    // carry that exact message, not a generic "tool_dispatch_error" marker.
    let resp = call_tool(
        server,
        9005,
        "tracedecay_str_replace",
        json!({
            "path": "src/main.rs",
            "old_str": "fn missing() {}",
            "new_str": "fn replaced() {}",
            "dry_run": true,
            "_meta": { "sessionId": "mcp-session-9005" }
        }),
    )
    .await;

    assert!(resp["error"].is_null(), "semantic failures are MCP results");
    assert_eq!(resp["result"]["isError"], true);

    server_handle.ledger_writes_settled().await;
    let event = expect_mcp_runtime_event(
        db_path,
        "tracedecay_str_replace",
        "mcp-session-9005",
        "durable structural-failure MCP runtime analytics event",
    )
    .await;
    let metadata = analytics_metadata(&event);

    assert_eq!(event.outcome.as_deref(), Some("error"));
    let failure_reason = metadata["failure_reason"]
        .as_str()
        .expect("failure_reason should be a string");
    assert!(
        failure_reason.contains("old_str not found"),
        "failure_reason should carry the anchor-mismatch message, got: {failure_reason}"
    );
}

/// Regression test for the empty-ledger bug: the savings ledger must record
/// **by default**, with no env opt-in. The holographic-fact-store commit
/// made the global DB opt-in via `TRACEDECAY_ENABLE_GLOBAL_DB`, which
/// silently disabled ledger writes for every default MCP-server install
/// (dashboards showed "no events yet" while lifetime counters kept growing
/// through the ungated CLI paths).
#[tokio::test]
async fn ledger_records_by_default_without_env_opt_in() {
    let _env_guard = SAVINGS_ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Simulate a real (non-cargo) launch: neither the opt-in nor the
    // cargo-test opt-out is present, so the default-on path is exercised.
    let _enable = EnvVarGuard::unset("TRACEDECAY_ENABLE_GLOBAL_DB");
    let _disable = EnvVarGuard::unset("TRACEDECAY_DISABLE_GLOBAL_DB");
    assert!(tracedecay::global_db::global_accounting_enabled());

    let fixture = setup_accounted_server().await;
    let (server, server_handle) = (fixture.server.clone(), fixture.server.clone());
    let (proj_tmp, db_path) = (&fixture.project, &fixture.global_db_path);

    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(9103),
            "tools/call",
            json!({
                "name": "tracedecay_search",
                "arguments": { "query": "hello" }
            }),
        )],
    )
    .await;

    let resp_str = responses
        .iter()
        .find(|r| parse_response(r)["id"] == 9103)
        .expect("should have a response for id=9103");
    let resp = parse_response(resp_str);
    assert!(resp["error"].is_null(), "search should not error");

    settled_ledger_total(&server_handle, db_path, proj_tmp.path(), 1).await;
}

/// The explicit opt-outs must still work: a falsy
/// `TRACEDECAY_ENABLE_GLOBAL_DB` or a truthy `TRACEDECAY_DISABLE_GLOBAL_DB`
/// disables global accounting.
#[test]
fn global_accounting_env_overrides() {
    let _env_guard = SAVINGS_ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    use tracedecay::global_db::{AccountingMode, global_accounting_mode};

    let _clear_enable = EnvVarGuard::unset("TRACEDECAY_ENABLE_GLOBAL_DB");
    let _clear_disable = EnvVarGuard::unset("TRACEDECAY_DISABLE_GLOBAL_DB");
    assert_eq!(global_accounting_mode(), AccountingMode::Default);
    assert!(global_accounting_mode().enabled());

    {
        let _disable = EnvVarGuard::set("TRACEDECAY_DISABLE_GLOBAL_DB", std::ffi::OsStr::new("1"));
        assert_eq!(global_accounting_mode(), AccountingMode::DisabledByEnv);
        // An explicit enable wins over the opt-out (the cargo-test default).
        let _enable = EnvVarGuard::set("TRACEDECAY_ENABLE_GLOBAL_DB", std::ffi::OsStr::new("1"));
        assert_eq!(global_accounting_mode(), AccountingMode::EnabledByEnv);
    }

    let _enable_falsy = EnvVarGuard::set("TRACEDECAY_ENABLE_GLOBAL_DB", std::ffi::OsStr::new("0"));
    assert_eq!(global_accounting_mode(), AccountingMode::DisabledByEnv);
    assert!(!global_accounting_mode().enabled());
}

/// A full-file read returns the entire file to the agent, so it must not
/// credit the lifetime counters: net saving = before - after, clamped at
/// zero. Guards against the historical bug where the counters accumulated
/// the gross "before" estimate even when the response carried the whole file.
#[tokio::test]
async fn full_file_read_credits_zero_net_savings() {
    let fixture = setup_accounted_savings_server().await;
    let (server, server_handle) = (fixture.server.clone(), fixture.server.clone());
    let (proj_tmp, db_path) = (&fixture.project, &fixture.global_db_path);
    let project_path = proj_tmp.path().to_path_buf();

    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(9101),
            "tools/call",
            json!({
                "name": "tracedecay_read",
                "arguments": { "file": "src/main.rs", "mode": "full" }
            }),
        )],
    )
    .await;

    let resp_str = responses
        .iter()
        .find(|r| parse_response(r)["id"] == 9101)
        .expect("should have a response for id=9101");
    let resp = parse_response(resp_str);
    assert!(resp["error"].is_null(), "read should not error");

    // The metrics line must prove the raw estimate was real (before > 0)
    // and that a full read delivers at least as much as it "saves".
    let (before, after) = parse_metrics_line(&resp).expect("metrics line present");
    assert!(before > 0, "raw-file estimate should be nonzero");
    assert!(
        after >= before,
        "full-file response ({after}) should be at least the raw estimate ({before})"
    );

    let total = settled_ledger_total(&server_handle, db_path, &project_path, 1).await;
    assert_eq!(
        total.saved_tokens, 0,
        "ledger must not count a full-file read as savings"
    );
    let runtime = tracedecay::application::host_admission::HostAdmissionTestRuntimeV1::profile(
        db_path.parent().expect("global db has a profile root"),
    )
    .await
    .expect("registered profile runtime opens at isolated path");
    assert_eq!(
        runtime.get_project_tokens(&project_path).await,
        0,
        "lifetime counter must not be credited with the gross before estimate"
    );
}

/// The lifetime counter and the ledger must agree: both credit the net
/// saving (before - after) per call, so after a single compressed call the
/// per-project counter equals the ledger total.
#[tokio::test]
async fn lifetime_counter_matches_ledger_net_savings() {
    let fixture = setup_accounted_savings_server().await;
    let (server, server_handle) = (fixture.server.clone(), fixture.server.clone());
    let (proj_tmp, db_path) = (&fixture.project, &fixture.global_db_path);
    let project_path = proj_tmp.path().to_path_buf();

    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(9102),
            "tools/call",
            json!({
                "name": "tracedecay_search",
                "arguments": { "query": "helper" }
            }),
        )],
    )
    .await;

    let resp_str = responses
        .iter()
        .find(|r| parse_response(r)["id"] == 9102)
        .expect("should have a response for id=9102");
    let resp = parse_response(resp_str);
    assert!(resp["error"].is_null(), "search should not error");

    let (before, after) = parse_metrics_line(&resp).expect("metrics line present");
    assert!(
        before > after,
        "compressed search should save tokens (before={before}, after={after})"
    );

    let total = settled_ledger_total(&server_handle, db_path, &project_path, 1).await;
    assert_eq!(
        total.saved_tokens,
        before - after,
        "ledger net saving must match the metrics line"
    );
    let runtime = tracedecay::application::host_admission::HostAdmissionTestRuntimeV1::profile(
        db_path.parent().expect("global db has a profile root"),
    )
    .await
    .expect("registered profile runtime opens at isolated path");
    assert_eq!(
        runtime.get_project_tokens(&project_path).await,
        total.saved_tokens,
        "lifetime counter must equal the ledger's net saving, not the gross before"
    );
}
