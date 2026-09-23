//! Tests for the `tracedecay_analytics` MCP tool: per-tool call/error tiers,
//! hint telemetry, the fact-store funnel, and automation run rollups over a
//! seeded `analytics_events` store.

#[cfg(feature = "test-transport")]
use serde_json::json;

#[cfg(feature = "test-transport")]
use crate::support::{
    extract_json, extract_text, handle_real_server_tool_call, handle_real_server_tool_call_raw,
    production_composition_fixture,
};
#[cfg(feature = "test-transport")]
use serde_json::Value;
#[cfg(feature = "test-transport")]
use tracedecay_global_db::AnalyticsEventInsert;
#[cfg(feature = "test-transport")]
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;
#[cfg(feature = "test-transport")]
use tracedecay_runtime_core::tracedecay::current_timestamp;

#[cfg(feature = "test-transport")]
fn tool_call_event(
    project_id: &str,
    tool_name: &str,
    outcome: &str,
    timestamp: i64,
) -> AnalyticsEventInsert {
    AnalyticsEventInsert {
        provider: "mcp".to_string(),
        project_id: project_id.to_string(),
        session_id: Some("s1".to_string()),
        timestamp,
        event_kind: "mcp_tool_call".to_string(),
        hook_name: None,
        tool_name: Some(tool_name.to_string()),
        tool_category: Some("exploration".to_string()),
        skill_name: None,
        hint_category: None,
        hint_id: None,
        outcome: Some(outcome.to_string()),
        metadata_json: None,
    }
}

#[cfg(feature = "test-transport")]
fn hint_event(
    project_id: &str,
    event_kind: &str,
    outcome: Option<&str>,
    timestamp: i64,
) -> AnalyticsEventInsert {
    AnalyticsEventInsert {
        provider: "codex".to_string(),
        project_id: project_id.to_string(),
        session_id: Some("hint-session".to_string()),
        timestamp,
        event_kind: event_kind.to_string(),
        hook_name: Some("PostToolUse".to_string()),
        tool_name: None,
        tool_category: None,
        skill_name: None,
        hint_category: Some("search".to_string()),
        hint_id: Some("hint-search-1".to_string()),
        outcome: outcome.map(str::to_string),
        metadata_json: None,
    }
}

#[cfg(feature = "test-transport")]
fn metric<'a>(metrics: &'a Value, name: &str) -> &'a Value {
    metrics
        .as_array()
        .unwrap_or_else(|| panic!("expected a metrics array, got {metrics}"))
        .iter()
        .find(|metric| metric["metric"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("missing metric {name} in {metrics}"))
}

#[cfg(feature = "test-transport")]
fn ledger_row(run_id: &str, task: &str, status: &str, started_at: i64) -> String {
    format!(
        "{{\"schema_version\":2,\"run_id\":\"{run_id}\",\"trigger\":\"scheduler\",\
         \"task\":\"{task}\",\"backend\":\"codex_app_server\",\"status\":\"{status}\",\
         \"accepted_count\":0,\"rejected_count\":0,\"started_at\":\"{started_at}\",\
         \"completed_at\":\"{started_at}\",\"completed_at_micros\":{}}}",
        started_at.saturating_mul(1_000_000)
    )
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn analytics_reports_tool_tiers_top_tools_and_zero_call_tools() {
    let fixture = production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");

    for _ in 0..2 {
        handle_real_server_tool_call(
            &server,
            "tracedecay_grep",
            json!({"pattern": "helper", "fixed_strings": true}),
        )
        .await;
    }
    let failed_grep = handle_real_server_tool_call_raw(&server, "tracedecay_grep", json!({})).await;
    assert!(
        failed_grep["error"].is_object(),
        "missing grep pattern must fail over production MCP: {failed_grep}"
    );
    handle_real_server_tool_call(&server, "tracedecay_fact_store_list", json!({})).await;
    server.ledger_writes_settled().await;

    // JSON response carries the same data in the typed shape the markdown
    // was rendered from.
    let json_res =
        handle_real_server_tool_call(&server, "tracedecay_analytics", json!({"format": "json"}))
            .await;
    let payload = extract_json(&json_res);
    // MCP tool calls are not observability envelopes, so the observatory read
    // stays the empty known window even after grep and fact-store calls.
    let observed_events = metric(&payload["observatory"]["metrics"], "observability_events");
    assert_eq!(payload["observatory"]["watermark"], "analytics:empty");
    assert_eq!(payload["observatory"]["current"], true);
    assert_eq!(observed_events["value"].as_f64(), Some(0.0));
    assert_eq!(observed_events["unit"], "events");
    assert_eq!(observed_events["coverage"]["state"], "known");
    assert_eq!(observed_events["coverage"]["observed"].as_u64(), Some(0));
    assert_eq!(observed_events["coverage"]["eligible"].as_u64(), Some(0));
    let provider_tokens = metric(&payload["costs"]["usage"], "provider_tokens");
    assert!(provider_tokens["value"].is_null());
    assert_eq!(
        provider_tokens["unavailable_reason"],
        "provider_usage_unavailable"
    );
    assert_eq!(provider_tokens["coverage"]["state"], "unknown");
    let tools = &payload["tools"];
    assert_eq!(tools["available"].as_bool(), Some(true));
    assert_eq!(tools["raw_distinct_event_name_count"].as_i64(), Some(2));
    assert_eq!(
        tools["called_available_defined_tool_count"].as_i64(),
        Some(2)
    );

    let tiers = tools["tiers"].as_array().expect("tiers array");
    let navigation = tiers
        .iter()
        .find(|tier| tier["tier"] == "navigation")
        .expect("navigation tier present");
    assert_eq!(navigation["calls"].as_i64(), Some(3));
    assert_eq!(navigation["errors"].as_i64(), Some(1));
    let memory = tiers
        .iter()
        .find(|tier| tier["tier"] == "memory")
        .expect("memory tier present");
    assert_eq!(memory["calls"].as_i64(), Some(1));
    assert_eq!(memory["errors"].as_i64(), Some(0));

    let top_tools = tools["top_tools"].as_array().expect("top_tools array");
    let grep = top_tools
        .iter()
        .find(|tool| tool["tool_name"] == "tracedecay_grep")
        .expect("tracedecay_grep in top_tools");
    assert_eq!(grep["calls"].as_i64(), Some(3));
    assert_eq!(grep["errors"].as_i64(), Some(1));
    assert_eq!(grep["tier"].as_str(), Some("navigation"));

    let zero_call = &tools["zero_call_available_defined_tools"];
    assert!(zero_call["count"].as_i64().unwrap_or(0) > 0);
    let sample = zero_call["sample"]
        .as_array()
        .expect("zero_call sample array");
    assert!(
        sample
            .iter()
            .any(|name| name == "tracedecay_active_project"),
        "tracedecay_active_project should show up as a zero-call tool: {sample:?}"
    );
    assert!(
        !sample
            .iter()
            .any(|name| name == "tracedecay_grep" || name == "tracedecay_fact_store_list"),
        "called tools must not appear in the zero-call sample: {sample:?}"
    );

    // Markdown response carries the tier/tool breakdown as human-readable
    // text. Run it after the count assertions because the real server records
    // its own tool calls asynchronously.
    let res = handle_real_server_tool_call(
        &server,
        "tracedecay_analytics",
        json!({"format": "markdown"}),
    )
    .await;
    let text = extract_text(&res);
    assert!(text.contains("Usage Analytics"), "missing heading: {text}");
    assert!(
        text.contains("navigation"),
        "missing navigation tier: {text}"
    );
    assert!(text.contains("memory"), "missing memory tier: {text}");
    assert!(text.contains("tracedecay_grep"), "missing top tool: {text}");
    assert!(
        text.contains("Zero-Call Available Defined Tools"),
        "missing zero-call section: {text}"
    );
    drop(server);
    fixture.harness.shutdown().await;
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn analytics_section_filter_returns_only_the_requested_section() {
    let fixture = production_composition_fixture().await;
    let project_id = HostAdmissionTestRuntimeV1::canonical_project_key(&fixture.project_root);
    let timestamp = current_timestamp() - 60;
    fixture
        .harness
        .append_profile_analytics_events_for_test(&[tool_call_event(
            &project_id,
            "tracedecay_grep",
            "error",
            timestamp,
        )])
        .await
        .expect("seeding one tool call should succeed");
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");

    let res = handle_real_server_tool_call(
        &server,
        "tracedecay_analytics",
        json!({"section": "tools", "format": "json"}),
    )
    .await;
    let payload = extract_json(&res);
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["scope"], "project");
    assert_eq!(payload["window_days"].as_i64(), Some(14));
    assert_eq!(payload["event_count"].as_i64(), Some(1));
    assert_eq!(payload["event_count_truncated"], false);
    assert_eq!(
        payload["tools"]["top_tools"],
        json!([{
            "tool_name": "tracedecay_grep",
            "tier": "navigation",
            "calls": 1,
            "errors": 1,
        }])
    );
    assert_eq!(
        payload["tools"]["tiers"],
        json!([{"tier": "navigation", "calls": 1, "errors": 1}])
    );
    for unrelated in ["hints", "facts", "automation", "observatory", "costs"] {
        assert!(
            payload.get(unrelated).is_none(),
            "sectioned analytics unexpectedly included {unrelated}: {payload}"
        );
    }
    drop(server);
    fixture.harness.shutdown().await;
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn analytics_rejects_unknown_scope_and_section() {
    let fixture = production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");

    let response = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_analytics",
        json!({"scope": "bogus"}),
    )
    .await;
    assert_eq!(response["error"]["code"].as_i64(), Some(-32603));
    assert_eq!(
        response["error"]["message"],
        "tool execution failed: config error: unknown scope for tracedecay_analytics: bogus (use 'project' or 'all')"
    );

    let response = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_analytics",
        json!({"section": "bogus"}),
    )
    .await;
    assert_eq!(response["error"]["code"].as_i64(), Some(-32603));
    assert_eq!(
        response["error"]["message"],
        "tool execution failed: config error: unknown section for tracedecay_analytics: bogus (use 'tools', 'hints', 'facts', or 'automation')"
    );
    drop(server);
    fixture.harness.shutdown().await;
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn analytics_degrades_gracefully_for_a_zero_data_project() {
    let fixture = production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");

    let res =
        handle_real_server_tool_call(&server, "tracedecay_analytics", json!({"format": "json"}))
            .await;
    let payload = extract_json(&res);

    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["scope"], "project");
    assert_eq!(payload["window_days"].as_i64(), Some(14));
    assert_eq!(payload["event_count"].as_i64(), Some(0));
    assert_eq!(payload["event_count_truncated"], false);
    assert_eq!(payload["tools"]["available"].as_bool(), Some(false));
    assert_eq!(
        payload["tools"]["raw_distinct_event_name_count"].as_i64(),
        Some(0)
    );
    assert_eq!(payload["tools"]["tiers"], json!([]));
    assert_eq!(payload["tools"]["top_tools"], json!([]));

    let observed_events = metric(&payload["observatory"]["metrics"], "observability_events");
    assert_eq!(payload["observatory"]["watermark"], "analytics:empty");
    assert_eq!(payload["observatory"]["current"], true);
    assert_eq!(observed_events["value"].as_f64(), Some(0.0));
    assert_eq!(observed_events["coverage"]["state"], "known");
    assert_eq!(observed_events["coverage"]["observed"].as_u64(), Some(0));
    assert_eq!(payload["costs"]["watermark"], "provider-usage:0;savings:0");
    assert_eq!(payload["costs"]["current"], false);
    let saved_tokens = metric(&payload["costs"]["usage"], "saved_tokens");
    assert_eq!(saved_tokens["value"].as_f64(), Some(0.0));
    assert_eq!(saved_tokens["unit"], "tokens");
    assert_eq!(saved_tokens["coverage"]["state"], "known");
    assert_eq!(saved_tokens["coverage"]["observed"].as_u64(), Some(0));
    assert_eq!(saved_tokens["coverage"]["eligible"].as_u64(), Some(0));
    let provider_tokens = metric(&payload["costs"]["usage"], "provider_tokens");
    assert!(provider_tokens["value"].is_null());
    assert_eq!(
        provider_tokens["unavailable_reason"],
        "provider_usage_unavailable"
    );
    assert_eq!(provider_tokens["coverage"]["state"], "unknown");
    let provider_cost = metric(&payload["costs"]["estimated_cost"], "provider_cost");
    assert!(provider_cost["value"].is_null());
    assert_eq!(
        provider_cost["unavailable_reason"],
        "provider_usage_unavailable"
    );

    // Hints are computed from the same (empty) durable event window: a real
    // zero, not an error.
    assert_eq!(payload["hints"]["available"].as_bool(), Some(true));
    assert_eq!(payload["hints"]["source"], "analytics_events");
    let search = payload["hints"]["by_category"]
        .as_array()
        .expect("by_category array")
        .iter()
        .find(|row| row["category"] == "search")
        .expect("search hint category");
    assert_eq!(
        search,
        &json!({
            "category": "search",
            "emitted": 0,
            "followed": 0,
            "ignored": 0,
            "suppressed": 0,
        })
    );

    // The fact-store funnel and automation ledger resolve to real, empty
    // data for a freshly initialized project rather than failing.
    let project_root = payload["project_root"]
        .as_str()
        .expect("project root")
        .to_string();
    assert_eq!(
        payload["facts"],
        json!({
            "available": true,
            "project_root": project_root,
            "facts": 0,
            "retrievals": 0,
            "facts_retrieved": 0,
            "helpful_feedback": 0,
            "unhelpful_feedback": 0,
            "facts_rated": 0,
        })
    );
    assert_eq!(payload["automation"]["available"], true);
    assert_eq!(
        payload["automation"]["records_considered"].as_i64(),
        Some(0)
    );
    assert_eq!(payload["automation"]["records_in_window"].as_i64(), Some(0));
    assert_eq!(payload["automation"]["records_truncated"], false);
    assert_eq!(payload["automation"]["by_job"], json!([]));
    let dashboard_root = payload["automation"]["dashboard_root"]
        .as_str()
        .expect("enrolled dashboard root")
        .to_string();

    let fact_content = "Analytics fact funnel records one committed fact.";
    let added = handle_real_server_tool_call(
        &server,
        "tracedecay_fact_store_add",
        json!({
            "content": fact_content,
            "category": "decision",
            "entities": ["analytics fact funnel"],
            "trust": 0.94,
            "source_label": "analytics-fact-funnel"
        }),
    )
    .await;
    let added = extract_json(&added);
    assert_eq!(added["outcome"], "committed");
    assert_eq!(added["result"]["disposition"], "added");
    assert_eq!(added["result"]["fact"]["fact"]["content"], fact_content);

    let dashboard_path = std::path::PathBuf::from(&dashboard_root);
    std::fs::create_dir_all(&dashboard_path).expect("dashboard root");
    let now = current_timestamp();
    let ledger = format!(
        "{}\n{}\n{}\n{}\n",
        ledger_row(
            "analytics-recent-success",
            "memory_curator",
            "succeeded",
            now - 60
        ),
        ledger_row(
            "analytics-recent-failure",
            "skill_writer",
            "failed",
            now - 120
        ),
        ledger_row("analytics-recent-queued", "user_job", "queued", now - 180),
        ledger_row(
            "analytics-stale-success",
            "session_reflector",
            "succeeded",
            now - 20 * 86_400
        ),
    );
    std::fs::write(dashboard_path.join("automation_runs.jsonl"), ledger)
        .expect("automation ledger");

    let facts = extract_json(
        &handle_real_server_tool_call(
            &server,
            "tracedecay_analytics",
            json!({"section": "facts", "format": "json"}),
        )
        .await,
    );
    assert_eq!(
        facts["facts"],
        json!({
            "available": true,
            "project_root": project_root,
            "facts": 1,
            "retrievals": 0,
            "facts_retrieved": 0,
            "helpful_feedback": 0,
            "unhelpful_feedback": 0,
            "facts_rated": 0,
        })
    );

    let automation = extract_json(
        &handle_real_server_tool_call(
            &server,
            "tracedecay_analytics",
            json!({"section": "automation", "format": "json"}),
        )
        .await,
    );
    assert_eq!(
        automation["automation"],
        json!({
            "available": true,
            "dashboard_root": dashboard_root,
            "records_considered": 4,
            "records_in_window": 3,
            "records_truncated": false,
            "by_job": [
                {
                    "job": "memory_curator",
                    "succeeded": 1,
                    "failed": 0,
                    "skipped": 0,
                    "other": 0,
                },
                {
                    "job": "skill_writer",
                    "succeeded": 0,
                    "failed": 1,
                    "skipped": 0,
                    "other": 0,
                },
                {
                    "job": "user_job",
                    "succeeded": 0,
                    "failed": 0,
                    "skipped": 0,
                    "other": 1,
                },
            ],
        })
    );

    drop(server);
    fixture.harness.shutdown().await;

    let markdown_fixture = production_composition_fixture().await;
    let markdown_server = markdown_fixture
        .harness
        .server(&markdown_fixture.project_root)
        .expect("production project server");
    let md_res = handle_real_server_tool_call(
        &markdown_server,
        "tracedecay_analytics",
        json!({"format": "markdown"}),
    )
    .await;
    let text = extract_text(&md_res);
    assert!(
        text.contains("_No MCP tool calls recorded in this window._"),
        "expected an empty-state note in markdown: {text}"
    );
    assert!(
        text.contains("**window_days:** 14"),
        "expected the default window in markdown: {text}"
    );
    assert!(
        text.contains("**event_count:** 0"),
        "expected the empty event count in markdown: {text}"
    );
    drop(markdown_server);
    markdown_fixture.harness.shutdown().await;
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn analytics_reconciles_public_catalog_with_alias_internal_and_unknown_or_retired_events() {
    let fixture = production_composition_fixture().await;
    let project_id = HostAdmissionTestRuntimeV1::canonical_project_key(&fixture.project_root);
    let timestamp = current_timestamp() - 60;
    let events = [
        tool_call_event(&project_id, "tracedecay_grep", "ok", timestamp),
        tool_call_event(&project_id, "grep", "error", timestamp),
        tool_call_event(
            &project_id,
            "mcp__tracedecay__tracedecay_context",
            "ok",
            timestamp,
        ),
        tool_call_event(&project_id, "tracedecay_admin_cli", "ok", timestamp),
        tool_call_event(&project_id, "tracedecay_removed_tool", "error", timestamp),
    ];
    fixture
        .harness
        .append_profile_analytics_events_for_test(&events)
        .await
        .expect("seeding analytics classification events should succeed");
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");

    let response = handle_real_server_tool_call(
        &server,
        "tracedecay_analytics",
        json!({"section": "tools", "format": "json"}),
    )
    .await;
    let payload = extract_json(&response);
    let tools = &payload["tools"];
    assert_eq!(tools["raw_distinct_event_name_count"].as_i64(), Some(5));
    for retired in [
        "distinct_tools_called",
        "defined_tool_count",
        "zero_call_tools",
    ] {
        assert!(
            tools.get(retired).is_none(),
            "retired analytics key {retired} must not be emitted"
        );
    }
    assert_eq!(
        tools["called_available_defined_tool_count"].as_i64(),
        Some(2)
    );
    let available_defined_tool_count = tools["available_defined_tool_count"]
        .as_i64()
        .expect("available defined tool count");
    let maximal_defined_tool_count = tools["maximal_defined_tool_count"]
        .as_i64()
        .expect("maximal defined tool count");
    let zero_call_count = tools["zero_call_available_defined_tools"]["count"]
        .as_i64()
        .expect("zero-call available defined tool count");
    assert_eq!(
        available_defined_tool_count,
        tools["called_available_defined_tool_count"]
            .as_i64()
            .expect("called available-defined tool count")
            + zero_call_count,
        "available catalog membership must partition into called and zero-call tools"
    );
    assert!(maximal_defined_tool_count >= available_defined_tool_count);

    assert_eq!(
        tools["aliased_call_names"],
        json!([
            {
                "event_name": "grep",
                "canonical_tool_name": "tracedecay_grep",
                "calls": 1,
                "errors": 1,
            },
            {
                "event_name": "mcp__tracedecay__tracedecay_context",
                "canonical_tool_name": "tracedecay_context",
                "calls": 1,
                "errors": 0,
            },
        ])
    );
    assert_eq!(
        tools["bound_internal_call_names"],
        json!([{
            "event_name": "tracedecay_admin_cli",
            "calls": 1,
            "errors": 0,
        }])
    );
    assert_eq!(
        tools["unknown_or_retired_call_names"],
        json!([{
            "event_name": "tracedecay_removed_tool",
            "calls": 1,
            "errors": 1,
        }])
    );
    assert_eq!(
        tools["tiers"],
        json!([
            {"tier": "navigation", "calls": 3, "errors": 1},
            {"tier": "other", "calls": 2, "errors": 1},
        ])
    );
    assert_eq!(
        tools["top_tools"],
        json!([
            {
                "tool_name": "tracedecay_grep",
                "tier": "navigation",
                "calls": 2,
                "errors": 1,
            },
            {
                "tool_name": "tracedecay_admin_cli",
                "tier": "other",
                "calls": 1,
                "errors": 0,
            },
            {
                "tool_name": "tracedecay_context",
                "tier": "navigation",
                "calls": 1,
                "errors": 0,
            },
            {
                "tool_name": "tracedecay_removed_tool",
                "tier": "other",
                "calls": 1,
                "errors": 1,
            },
        ])
    );

    server.ledger_writes_settled().await;
    let markdown = handle_real_server_tool_call(
        &server,
        "tracedecay_analytics",
        json!({"section": "tools", "format": "markdown"}),
    )
    .await;
    let text = extract_text(&markdown);
    for line in [
        "**event_count:** 6",
        "**raw distinct event names:** 6",
        "**called available defined tools:** 3",
        "- **navigation** - 3 calls, 1 errors",
        "- **admin** - 1 calls, 0 errors",
        "- **other** - 2 calls, 1 errors",
        "- **tracedecay_grep** (navigation) - 2 calls, 1 errors",
        "- **tracedecay_analytics** (admin) - 1 calls, 0 errors",
        "- **tracedecay_admin_cli** (other) - 1 calls, 0 errors",
        "- **tracedecay_context** (navigation) - 1 calls, 0 errors",
        "- **tracedecay_removed_tool** (other) - 1 calls, 1 errors",
        "- **grep** → **tracedecay_grep** - 1 calls, 1 errors",
        "- **mcp__tracedecay__tracedecay_context** → **tracedecay_context** - 1 calls, 0 errors",
        "- **tracedecay_admin_cli** - 1 calls, 0 errors",
        "- **tracedecay_removed_tool** - 1 calls, 1 errors",
        "#### Unavailable Public Call Names\n_None._",
    ] {
        assert!(text.contains(line), "missing `{line}` in:\n{text}");
    }

    drop(server);
    fixture.harness.shutdown().await;
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn analytics_keeps_host_unavailable_public_routes_out_of_internal_calls() {
    let fixture = production_composition_fixture().await;
    let project_id = HostAdmissionTestRuntimeV1::canonical_project_key(&fixture.project_root);
    let timestamp = current_timestamp() - 60;
    let event = tool_call_event(
        &project_id,
        "tracedecay_ast_grep_rewrite",
        "error",
        timestamp,
    );
    fixture
        .harness
        .append_profile_analytics_events_for_test(&[event])
        .await
        .expect("seeding a maximal public route event should succeed");
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");

    let response = handle_real_server_tool_call(
        &server,
        "tracedecay_analytics",
        json!({"section": "tools", "format": "json"}),
    )
    .await;
    let payload = extract_json(&response);
    let tools = &payload["tools"];
    let ast_grep_rewrite_is_available = tracedecay_mcp::get_tool_definitions()
        .expect("available tool definitions")
        .iter()
        .any(|definition| definition.name == "tracedecay_ast_grep_rewrite");

    if ast_grep_rewrite_is_available {
        assert_eq!(
            tools["called_available_defined_tool_count"].as_i64(),
            Some(1)
        );
        assert_eq!(tools["unavailable_public_call_names"], json!([]));
    } else {
        assert_eq!(
            tools["called_available_defined_tool_count"].as_i64(),
            Some(0)
        );
        assert_eq!(
            tools["unavailable_public_call_names"],
            json!([{
                "event_name": "tracedecay_ast_grep_rewrite",
                "canonical_tool_name": "tracedecay_ast_grep_rewrite",
                "calls": 1,
                "errors": 1,
            }])
        );
    }
    assert_eq!(tools["bound_internal_call_names"], json!([]));
    assert_eq!(tools["unknown_or_retired_call_names"], json!([]));

    drop(server);
    fixture.harness.shutdown().await;
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn analytics_aggregates_sections_before_any_event_sample_cap() {
    let fixture = production_composition_fixture().await;
    let project_id = HostAdmissionTestRuntimeV1::canonical_project_key(&fixture.project_root);
    let timestamp = current_timestamp() - 60;

    let events = vec![
        hint_event(&project_id, "hint_emitted", None, timestamp),
        hint_event(&project_id, "hint_outcome", Some("acted"), timestamp),
        hint_event(&project_id, "hint_outcome", Some("ignored"), timestamp),
        hint_event(&project_id, "suppressed_duplicate", None, timestamp),
        tool_call_event(&project_id, "tracedecay_grep", "ok", timestamp),
    ];
    fixture
        .harness
        .append_profile_analytics_events_for_test(&events)
        .await
        .expect("seeding a busy analytics window should succeed");
    let unrelated_event = AnalyticsEventInsert {
        provider: "codex".to_string(),
        project_id: project_id.clone(),
        session_id: Some("busy-session".to_string()),
        timestamp,
        event_kind: "hook_completed".to_string(),
        hook_name: Some("PostToolUse".to_string()),
        tool_name: None,
        tool_category: None,
        skill_name: None,
        hint_category: None,
        hint_id: None,
        outcome: Some("observed".to_string()),
        metadata_json: None,
    };
    fixture
        .harness
        .append_profile_analytics_events_for_test(&vec![unrelated_event; 10_001])
        .await
        .expect("seed more than ten thousand unrelated newer events");
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");

    let response =
        handle_real_server_tool_call(&server, "tracedecay_analytics", json!({"format": "json"}))
            .await;
    let payload = extract_json(&response);
    assert_eq!(payload["event_count"].as_i64(), Some(10_006));
    assert_eq!(payload["event_count_truncated"].as_bool(), Some(false));
    let search = payload["hints"]["by_category"]
        .as_array()
        .expect("hint categories")
        .iter()
        .find(|row| row["category"] == "search")
        .expect("search hint category");
    assert_eq!(
        search,
        &json!({
            "category": "search",
            "emitted": 1,
            "followed": 1,
            "ignored": 1,
            "suppressed": 1,
        })
    );

    assert_eq!(
        payload["tools"]["raw_distinct_event_name_count"].as_i64(),
        Some(1)
    );
    assert_eq!(
        payload["tools"]["called_available_defined_tool_count"].as_i64(),
        Some(1)
    );
    assert_eq!(
        payload["tools"]["top_tools"][0]["tool_name"],
        "tracedecay_grep"
    );
    assert_eq!(payload["tools"]["top_tools"][0]["calls"].as_i64(), Some(1));

    let markdown = handle_real_server_tool_call(
        &server,
        "tracedecay_analytics",
        json!({"section": "hints", "format": "markdown"}),
    )
    .await;
    let text = extract_text(&markdown);
    assert!(
        text.contains("- **search** - emitted 1, followed 1, ignored 1, suppressed 1"),
        "missing the search hint line in:\n{text}"
    );
    drop(server);
    fixture.harness.shutdown().await;
}

#[cfg(feature = "test-transport")]
fn active_window_events(project_id: &str, now: i64) -> Vec<AnalyticsEventInsert> {
    vec![
        tool_call_event(project_id, "tracedecay_grep", "ok", now - 60),
        tool_call_event(
            "project.foreign.analytics",
            "tracedecay_fact_store_list",
            "error",
            now - 60,
        ),
        tool_call_event(project_id, "tracedecay_dead_code", "ok", now - 20 * 86_400),
    ]
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn analytics_keeps_foreign_and_stale_events_out_of_the_active_window() {
    let fixture = production_composition_fixture().await;
    let project_id = HostAdmissionTestRuntimeV1::canonical_project_key(&fixture.project_root);
    let now = current_timestamp();
    fixture
        .harness
        .append_profile_analytics_events_for_test(&active_window_events(&project_id, now))
        .await
        .expect("seeding mixed-scope analytics events should succeed");
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");

    let payload = extract_json(
        &handle_real_server_tool_call(
            &server,
            "tracedecay_analytics",
            json!({"section": "tools", "format": "json"}),
        )
        .await,
    );
    assert_eq!(payload["scope"], "project");
    assert_eq!(payload["project_id"], project_id);
    assert_eq!(payload["window_days"].as_i64(), Some(14));
    assert_eq!(payload["event_count"].as_i64(), Some(1));
    assert_eq!(
        payload["tools"]["tiers"],
        json!([{"tier": "navigation", "calls": 1, "errors": 0}])
    );
    assert_eq!(
        payload["tools"]["top_tools"],
        json!([{
            "tool_name": "tracedecay_grep",
            "tier": "navigation",
            "calls": 1,
            "errors": 0,
        }])
    );
    drop(server);
    fixture.harness.shutdown().await;
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn analytics_scope_all_counts_foreign_project_events_and_keeps_project_facts() {
    let fixture = production_composition_fixture().await;
    let project_id = HostAdmissionTestRuntimeV1::canonical_project_key(&fixture.project_root);
    let now = current_timestamp();
    fixture
        .harness
        .append_profile_analytics_events_for_test(&active_window_events(&project_id, now))
        .await
        .expect("seeding mixed-scope analytics events should succeed");
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");

    let payload = extract_json(
        &handle_real_server_tool_call(
            &server,
            "tracedecay_analytics",
            json!({"scope": "all", "window_days": 14, "format": "json"}),
        )
        .await,
    );
    assert_eq!(payload["scope"], "all");
    assert!(payload["project_id"].is_null());
    assert_eq!(payload["window_days"].as_i64(), Some(14));
    assert_eq!(payload["event_count"].as_i64(), Some(2));
    assert_eq!(
        payload["tools"]["tiers"],
        json!([
            {"tier": "navigation", "calls": 1, "errors": 0},
            {"tier": "memory", "calls": 1, "errors": 1},
        ])
    );
    assert_eq!(
        payload["tools"]["top_tools"],
        json!([
            {
                "tool_name": "tracedecay_fact_store_list",
                "tier": "memory",
                "calls": 1,
                "errors": 1,
            },
            {
                "tool_name": "tracedecay_grep",
                "tier": "navigation",
                "calls": 1,
                "errors": 0,
            },
        ])
    );
    assert_eq!(payload["facts"]["available"], true);
    assert_eq!(payload["facts"]["facts"].as_i64(), Some(0));
    assert_eq!(payload["facts"]["project_root"], payload["project_root"]);
    assert_eq!(payload["automation"]["records_in_window"].as_i64(), Some(0));
    drop(server);
    fixture.harness.shutdown().await;
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn analytics_clamps_window_days_and_applies_the_clamped_window() {
    let fixture = production_composition_fixture().await;
    let project_id = HostAdmissionTestRuntimeV1::canonical_project_key(&fixture.project_root);
    let now = current_timestamp();
    fixture
        .harness
        .append_profile_analytics_events_for_test(&active_window_events(&project_id, now))
        .await
        .expect("seeding mixed-scope analytics events should succeed");
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");

    let wide = extract_json(
        &handle_real_server_tool_call(
            &server,
            "tracedecay_analytics",
            json!({"section": "tools", "window_days": 400, "format": "json"}),
        )
        .await,
    );
    assert_eq!(wide["window_days"].as_i64(), Some(365));
    assert_eq!(wide["event_count"].as_i64(), Some(2));
    assert_eq!(
        wide["tools"]["top_tools"],
        json!([
            {
                "tool_name": "tracedecay_dead_code",
                "tier": "analysis",
                "calls": 1,
                "errors": 0,
            },
            {
                "tool_name": "tracedecay_grep",
                "tier": "navigation",
                "calls": 1,
                "errors": 0,
            },
        ])
    );

    server.ledger_writes_settled().await;
    let narrow = extract_json(
        &handle_real_server_tool_call(
            &server,
            "tracedecay_analytics",
            json!({"section": "tools", "window_days": 0, "format": "json"}),
        )
        .await,
    );
    assert_eq!(narrow["window_days"].as_i64(), Some(1));
    assert_eq!(narrow["event_count"].as_i64(), Some(2));
    assert_eq!(
        narrow["tools"]["top_tools"],
        json!([
            {
                "tool_name": "tracedecay_analytics",
                "tier": "admin",
                "calls": 1,
                "errors": 0,
            },
            {
                "tool_name": "tracedecay_grep",
                "tier": "navigation",
                "calls": 1,
                "errors": 0,
            },
        ])
    );
    drop(server);
    fixture.harness.shutdown().await;
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn analytics_limits_top_tools_to_the_ten_highest_call_counts() {
    let fixture = production_composition_fixture().await;
    let project_id = HostAdmissionTestRuntimeV1::canonical_project_key(&fixture.project_root);
    let timestamp = current_timestamp() - 60;
    let mut events = Vec::new();
    for index in 0..11 {
        let calls = 11 - index;
        let name = format!("tracedecay_rank_{index:02}");
        for call in 0..calls {
            let outcome = if index == 0 && call == 0 {
                "error"
            } else {
                "ok"
            };
            events.push(tool_call_event(&project_id, &name, outcome, timestamp));
        }
    }
    fixture
        .harness
        .append_profile_analytics_events_for_test(&events)
        .await
        .expect("seeding ranked tool calls should succeed");
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");

    let payload = extract_json(
        &handle_real_server_tool_call(
            &server,
            "tracedecay_analytics",
            json!({"section": "tools", "format": "json"}),
        )
        .await,
    );
    let top_tools = payload["tools"]["top_tools"]
        .as_array()
        .expect("top_tools array");
    assert_eq!(top_tools.len(), 10);
    assert_eq!(
        top_tools[0],
        json!({
            "tool_name": "tracedecay_rank_00",
            "tier": "other",
            "calls": 11,
            "errors": 1,
        })
    );
    assert_eq!(
        top_tools[9],
        json!({
            "tool_name": "tracedecay_rank_09",
            "tier": "other",
            "calls": 2,
            "errors": 0,
        })
    );
    assert!(
        top_tools
            .iter()
            .all(|tool| tool["tool_name"] != "tracedecay_rank_10"),
        "the eleventh tool by call volume must be omitted: {top_tools:?}"
    );
    assert_eq!(
        payload["tools"]["raw_distinct_event_name_count"].as_i64(),
        Some(11)
    );
    drop(server);
    fixture.harness.shutdown().await;
}
