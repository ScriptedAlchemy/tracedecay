//! Tests for the `tracedecay_analytics` MCP tool: per-tool call/error tiers,
//! hint telemetry, the fact-store funnel, and automation run rollups over a
//! seeded `analytics_events` store.

use serde_json::{Value, json};

use tracedecay::global_db::{AnalyticsEventInsert, GlobalDb, global_db_path};
use tracedecay::mcp::handle_tool_call;
use tracedecay::tracedecay::current_timestamp;

fn extract_text(v: &Value) -> String {
    v.get("content")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|t| t.get("text"))
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string()
}

fn extract_json(v: &Value) -> Value {
    serde_json::from_str(&extract_text(v)).expect("tool response should be valid JSON")
}

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

#[tokio::test]
async fn analytics_reports_tool_tiers_top_tools_and_zero_call_tools() {
    let (cg, _env) = crate::mcp_handler_test::setup_project().await;
    let project_id = GlobalDb::canonical_project_key(cg.project_root());
    let now = current_timestamp();

    let gdb = GlobalDb::open()
        .await
        .expect("isolated test global db should open");
    gdb.append_analytics_events(&[
        tool_call_event(&project_id, "tracedecay_grep", "ok", now - 60),
        tool_call_event(&project_id, "tracedecay_grep", "ok", now - 50),
        tool_call_event(&project_id, "tracedecay_grep", "error", now - 40),
        tool_call_event(&project_id, "tracedecay_fact_store", "ok", now - 30),
    ])
    .await
    .expect("seeding analytics events should succeed");

    // Markdown (default) response carries the tier/tool breakdown as
    // human-readable text.
    let res = handle_tool_call(&cg, "tracedecay_analytics", json!({}), None, None)
        .await
        .expect("tracedecay_analytics should succeed");
    let text = extract_text(&res.value);
    assert!(text.contains("Usage Analytics"), "missing heading: {text}");
    assert!(
        text.contains("navigation"),
        "missing navigation tier: {text}"
    );
    assert!(text.contains("memory"), "missing memory tier: {text}");
    assert!(text.contains("tracedecay_grep"), "missing top tool: {text}");
    assert!(
        text.contains("Zero-Call Defined Tools"),
        "missing zero-call section: {text}"
    );

    // JSON response carries the same data in the typed shape the markdown
    // was rendered from.
    let json_res = handle_tool_call(
        &cg,
        "tracedecay_analytics",
        json!({"format": "json"}),
        None,
        None,
    )
    .await
    .expect("json format should succeed");
    let payload = extract_json(&json_res.value);
    let tools = &payload["tools"];
    assert_eq!(tools["available"].as_bool(), Some(true));
    assert_eq!(tools["distinct_tools_called"].as_i64(), Some(2));

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

    let zero_call = &tools["zero_call_tools"];
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
            .any(|name| name == "tracedecay_grep" || name == "tracedecay_fact_store"),
        "called tools must not appear in the zero-call sample: {sample:?}"
    );
}

#[tokio::test]
async fn analytics_section_filter_returns_only_the_requested_section() {
    let (cg, _env) = crate::mcp_handler_test::setup_project().await;

    let res = handle_tool_call(
        &cg,
        "tracedecay_analytics",
        json!({"section": "facts", "format": "json"}),
        None,
        None,
    )
    .await
    .expect("tracedecay_analytics with section=facts should succeed");
    let payload = extract_json(&res.value);
    assert!(payload.get("facts").is_some(), "facts section missing");
    assert!(
        payload.get("tools").is_none(),
        "tools section should be omitted"
    );
    assert!(
        payload.get("hints").is_none(),
        "hints section should be omitted"
    );
    assert!(
        payload.get("automation").is_none(),
        "automation section should be omitted"
    );
}

#[tokio::test]
async fn analytics_rejects_unknown_scope_and_section() {
    let (cg, _env) = crate::mcp_handler_test::setup_project().await;

    let err = handle_tool_call(
        &cg,
        "tracedecay_analytics",
        json!({"scope": "bogus"}),
        None,
        None,
    )
    .await
    .expect_err("unknown scope should be rejected");
    assert!(err.to_string().contains("scope"), "unexpected error: {err}");

    let err = handle_tool_call(
        &cg,
        "tracedecay_analytics",
        json!({"section": "bogus"}),
        None,
        None,
    )
    .await
    .expect_err("unknown section should be rejected");
    assert!(
        err.to_string().contains("section"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn analytics_degrades_gracefully_for_a_zero_data_project() {
    let (cg, _env) = crate::mcp_handler_test::setup_project().await;

    let res = handle_tool_call(
        &cg,
        "tracedecay_analytics",
        json!({"format": "json"}),
        None,
        None,
    )
    .await
    .expect("tracedecay_analytics should succeed even with no recorded activity");
    let payload = extract_json(&res.value);

    assert_eq!(payload["event_count"].as_i64(), Some(0));
    assert_eq!(payload["tools"]["available"].as_bool(), Some(false));

    // Hints are computed from the same (empty) durable event window: a real
    // zero, not an error.
    assert_eq!(payload["hints"]["available"].as_bool(), Some(true));
    let by_category = payload["hints"]["by_category"]
        .as_array()
        .expect("by_category array");
    assert!(
        by_category
            .iter()
            .all(|row| row["emitted"].as_i64() == Some(0)),
        "expected zero hint counts for an empty window: {by_category:?}"
    );

    // The fact-store funnel and automation ledger resolve to real, empty
    // data for a freshly initialized project rather than failing.
    assert_eq!(payload["facts"]["available"].as_bool(), Some(true));
    assert_eq!(payload["facts"]["facts"].as_i64(), Some(0));
    assert_eq!(payload["automation"]["available"].as_bool(), Some(true));
    assert_eq!(payload["automation"]["records_in_window"].as_i64(), Some(0));

    let md_res = handle_tool_call(&cg, "tracedecay_analytics", json!({}), None, None)
        .await
        .expect("markdown format should also succeed with no data");
    let text = extract_text(&md_res.value);
    assert!(
        text.contains("No MCP tool calls recorded"),
        "expected an empty-state note in markdown: {text}"
    );
}

#[tokio::test]
async fn analytics_aggregates_sections_before_any_event_sample_cap() {
    let (cg, _env) = crate::mcp_handler_test::setup_project().await;
    let project_id = GlobalDb::canonical_project_key(cg.project_root());
    let timestamp = current_timestamp() - 60;
    let gdb = GlobalDb::open()
        .await
        .expect("isolated test global db should open");

    let events = vec![
        hint_event(&project_id, "hint_emitted", None, timestamp),
        hint_event(&project_id, "hint_outcome", Some("acted"), timestamp),
        hint_event(&project_id, "suppressed_duplicate", None, timestamp),
        tool_call_event(&project_id, "tracedecay_grep", "ok", timestamp),
    ];
    gdb.append_analytics_events(&events)
        .await
        .expect("seeding a busy analytics window should succeed");
    let db = libsql::Builder::new_local(global_db_path().expect("test global DB path"))
        .build()
        .await
        .expect("open test global DB directly");
    let conn = db.connect().expect("connect test global DB");
    conn.execute(
        "WITH RECURSIVE seq(n) AS (
             VALUES(0)
             UNION ALL
             SELECT n + 1 FROM seq WHERE n < 100
         )
         INSERT INTO analytics_events
             (provider, project_id, session_id, timestamp, event_kind, hook_name, outcome)
         SELECT 'codex', ?1, 'busy-session', ?2, 'hook_completed', 'PostToolUse', 'observed'
         FROM seq AS left_seq CROSS JOIN seq AS right_seq
         LIMIT 10001",
        libsql::params![project_id.as_str(), timestamp],
    )
    .await
    .expect("seed more than ten thousand unrelated newer events");

    let hint_response = handle_tool_call(
        &cg,
        "tracedecay_analytics",
        json!({"section": "hints", "format": "json"}),
        None,
        None,
    )
    .await
    .expect("hint analytics should succeed");
    let hints = extract_json(&hint_response.value);
    assert_eq!(hints["event_count"].as_i64(), Some(10_005));
    assert_eq!(hints["event_count_truncated"].as_bool(), Some(false));
    let search = hints["hints"]["by_category"]
        .as_array()
        .expect("hint categories")
        .iter()
        .find(|row| row["category"] == "search")
        .expect("search hint category");
    assert_eq!(search["emitted"].as_i64(), Some(1));
    assert_eq!(search["followed"].as_i64(), Some(1));
    assert_eq!(search["suppressed"].as_i64(), Some(1));

    let tool_response = handle_tool_call(
        &cg,
        "tracedecay_analytics",
        json!({"section": "tools", "format": "json"}),
        None,
        None,
    )
    .await
    .expect("tool analytics should succeed");
    let tools = extract_json(&tool_response.value);
    assert_eq!(tools["tools"]["distinct_tools_called"].as_i64(), Some(1));
    assert_eq!(
        tools["tools"]["top_tools"][0]["tool_name"],
        "tracedecay_grep"
    );
    assert_eq!(tools["tools"]["top_tools"][0]["calls"].as_i64(), Some(1));
}
