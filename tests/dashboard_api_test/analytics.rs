//! Integration tests for dashboard durable analytics endpoints.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use crate::common::{
    EnvVarGuard, GLOBAL_DB_ENV_LOCK as ENV_LOCK, MessageRecordBuilder, create_runtime, get_json,
    http_agent, pick_free_port, wait_for_dashboard,
};
use crate::runtime::DashboardTestRuntimeV1;
use serde_json::Value;
use std::sync::Arc;
use tempfile::TempDir;
use tracedecay::application::host_admission::HostAdmissionScope;
use tracedecay::config::USER_DATA_DIR_ENV;
use tracedecay::dashboard;
use tracedecay::global_db::AnalyticsEventInsert;
use tracedecay::sessions::{SessionMessageRecord, SessionRecord};
use tracedecay_domain::{
    CoverageStateV1, ObservabilityEnvelopeV1, ObservabilityPayloadV1,
    ObservabilityRetentionClassV1, ObservabilityTerminalResultV1, RetrievalQueryObservedV1,
};

struct Fixture {
    _tmp: TempDir,
    _env_guard: EnvVarGuard,
    _data_dir_guard: EnvVarGuard,
    base_url: String,
    server: tokio::task::JoinHandle<()>,
    project_root: PathBuf,
    store_root: PathBuf,
    host_runtime: Arc<DashboardTestRuntimeV1>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn session(project: &Path) -> SessionRecord {
    SessionRecord {
        provider: "codex".to_string(),
        session_id: "analytics-session".to_string(),
        project_key: "analytics-fixture".to_string(),
        project_path: project.display().to_string(),
        title: Some("Analytics fixture".to_string()),
        started_at: Some(1_760_000_000),
        ended_at: None,
        transcript_path: None,
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    }
}

fn subagent_session(project: &Path, id: &str, agent_id: &str) -> SessionRecord {
    SessionRecord {
        provider: "codex".to_string(),
        session_id: id.to_string(),
        project_key: "analytics-fixture".to_string(),
        project_path: project.display().to_string(),
        title: Some(format!("Subagent {agent_id}")),
        started_at: Some(1_760_000_010),
        ended_at: None,
        transcript_path: None,
        metadata_json: None,
        parent_session_id: Some("analytics-session".to_string()),
        is_subagent: true,
        agent_id: Some(agent_id.to_string()),
        parent_tool_use_id: None,
    }
}

fn subagent_session_with_metadata(
    project: &Path,
    id: &str,
    agent_id: &str,
    metadata_json: Value,
) -> SessionRecord {
    SessionRecord {
        metadata_json: Some(metadata_json.to_string()),
        ..subagent_session(project, id, agent_id)
    }
}

fn message(
    id: &str,
    role: &str,
    ordinal: i64,
    text: &str,
    kind: &str,
    tool_names: Option<&str>,
    metadata_json: Option<&str>,
) -> SessionMessageRecord {
    MessageRecordBuilder::new("codex", id, "analytics-session", role, ordinal, text, kind)
        .with_timestamp(Some(1_760_000_000 + ordinal))
        .with_model(Some("gpt-5.5"))
        .with_tool_names(tool_names)
        .with_metadata(metadata_json)
        .build()
}

async fn seed_session_store(runtime: &DashboardTestRuntimeV1, project: &Path) {
    assert!(
        runtime
            .upsert_session_for_test(HostAdmissionScope::Project, &session(project))
            .await
            .expect("seed session")
    );
    assert!(
        runtime
            .upsert_session_for_test(
                HostAdmissionScope::Project,
                &subagent_session(
                    project,
                    "subagent-code-explorer",
                    "tracedecay-code-explorer",
                ),
            )
            .await
            .expect("seed code explorer subagent")
    );
    assert!(
        runtime
            .upsert_session_for_test(
                HostAdmissionScope::Project,
                &subagent_session(project, "subagent-session-historian", "session-historian",),
            )
            .await
            .expect("seed session historian subagent")
    );
    assert!(
        runtime
            .upsert_session_for_test(
                HostAdmissionScope::Project,
                &subagent_session(project, "subagent-worker", "worker"),
            )
            .await
            .expect("seed worker subagent")
    );
    assert!(
        runtime
            .upsert_session_for_test(
                HostAdmissionScope::Project,
                &subagent_session_with_metadata(
                    project,
                    "subagent-code-health-auditor",
                    "auditor",
                    serde_json::json!({
                        "agent_nickname": "tracedecay-code-health-auditor",
                        "agent_role": "auditor"
                    }),
                ),
            )
            .await
            .expect("seed code health auditor subagent")
    );

    let rows = [
        message(
            "msg-1",
            "assistant",
            1,
            "Using tracedecay:exploring-code before shell search.",
            "message",
            Some("mcp__tracedecay__tracedecay_context,mcp__tracedecay__tracedecay_search"),
            Some(r#"{"skills":["tracedecay:exploring-code"]}"#),
        ),
        message(
            "msg-2",
            "assistant",
            2,
            "Falling back to rg for a literal route path.",
            "tool_use",
            Some("Bash,rg"),
            None,
        ),
        message(
            "msg-3",
            "assistant",
            3,
            "Reading one file after indexed context.",
            "tool_use",
            Some("Read"),
            None,
        ),
        message(
            "msg-4",
            "assistant",
            4,
            "Applying focused route edits.",
            "tool_use",
            Some("apply_patch"),
            None,
        ),
    ];

    for row in rows {
        assert!(
            runtime
                .upsert_session_message_for_test(HostAdmissionScope::Project, &row)
                .await
                .expect("seed analytics session message")
        );
    }
}

fn analytics_event(project_id: &str, timestamp: i64, event_kind: &str) -> AnalyticsEventInsert {
    AnalyticsEventInsert {
        provider: "codex".to_string(),
        project_id: project_id.to_string(),
        session_id: Some("analytics-session".to_string()),
        timestamp,
        event_kind: event_kind.to_string(),
        hook_name: None,
        tool_name: None,
        tool_category: None,
        skill_name: None,
        hint_category: None,
        hint_id: None,
        outcome: None,
        metadata_json: None,
    }
}

fn observability_event(
    project_id: &str,
    timestamp: i64,
    terminal_result: ObservabilityTerminalResultV1,
) -> AnalyticsEventInsert {
    let envelope = ObservabilityEnvelopeV1 {
        event_id: "dashboard-observability-failed".to_string(),
        event_kind: "retrieval.query.observed.v1".to_string(),
        schema_revision: 1,
        idempotency_key: "dashboard-observability-failed".to_string(),
        trace_id: "dashboard-observability-trace".to_string(),
        scope_ref: project_id.to_string(),
        capability: "retrieval".to_string(),
        operation: "query".to_string(),
        event_time_micros: timestamp.saturating_mul(1_000_000),
        observation_time_micros: timestamp.saturating_mul(1_000_000),
        valid_from_micros: None,
        valid_until_micros: None,
        quantity: None,
        unit: None,
        terminal_result: Some(terminal_result),
        producer_revision: "dashboard-test.v1".to_string(),
        configuration_revision: "dashboard-test.v1".to_string(),
        policy_revision: "dashboard-test.v1".to_string(),
        watermark: "dashboard-test:1".to_string(),
        coverage: CoverageStateV1::Known,
        sampling_probability: None,
        retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
        emitted_count: 1,
        delayed_count: 0,
        dropped_count: 0,
        process_boot_id: "dashboard-test-boot".to_string(),
        producer_sequence: 1,
        payload: ObservabilityPayloadV1::RetrievalQuery(RetrievalQueryObservedV1 {
            query_family: "exact_technical".to_string(),
            enabled_lanes: vec!["exact_literal".to_string()],
            candidate_budget: 1,
            context_budget: 1,
            token_budget: 1,
            answered: false,
            source_coverage: CoverageStateV1::Known,
            lane_coverage: CoverageStateV1::Known,
        }),
    };
    AnalyticsEventInsert {
        provider: "tracedecay-observability".to_string(),
        project_id: project_id.to_string(),
        session_id: None,
        timestamp,
        event_kind: envelope.event_kind.clone(),
        hook_name: None,
        tool_name: None,
        tool_category: None,
        skill_name: None,
        hint_category: None,
        hint_id: Some(envelope.idempotency_key.clone()),
        outcome: Some("failed".to_string()),
        metadata_json: Some(
            serde_json::to_string(&envelope).expect("serialize observability event"),
        ),
    }
}

async fn seed_durable_analytics(runtime: &DashboardTestRuntimeV1, project_root: &Path) {
    let project_id = DashboardTestRuntimeV1::canonical_project_key(project_root);
    let rows = [
        AnalyticsEventInsert {
            hint_category: Some("search".to_string()),
            hint_id: Some("hint-search".to_string()),
            outcome: Some("observed".to_string()),
            ..analytics_event(&project_id, 1_760_000_100, "hint_emitted")
        },
        AnalyticsEventInsert {
            tool_name: Some("mcp__tracedecay__tracedecay_context".to_string()),
            tool_category: Some("mcp".to_string()),
            outcome: Some("success".to_string()),
            ..analytics_event(&project_id, 1_760_000_101, "mcp_tool_call")
        },
        AnalyticsEventInsert {
            skill_name: Some("superpowers:test-driven-development".to_string()),
            outcome: Some("used".to_string()),
            ..analytics_event(&project_id, 1_760_000_102, "skill")
        },
        AnalyticsEventInsert {
            tool_name: Some("mcp__tracedecay__tracedecay_context".to_string()),
            tool_category: Some("mcp".to_string()),
            outcome: Some("success".to_string()),
            ..analytics_event("other-project", 1_760_000_103, "mcp_tool_call")
        },
    ];
    for row in rows {
        runtime
            .append_analytics_event_for_test(HostAdmissionScope::Profile, &row)
            .await
            .expect("append durable analytics event");
    }
}

fn seed_hook_analytics(store_root: &Path) {
    std::fs::create_dir_all(store_root).expect("create store root");
    let rows = [
        serde_json::json!({
            "event": "hook_invoked",
            "ts_unix_ms": 1_760_000_300_000u64,
            "agent": "codex",
            "hook_name": "UserPromptSubmit",
            "session_id": "analytics-session",
            "tool_name": null,
            "prompt_category": "dashboard_or_ui",
        }),
        serde_json::json!({
            "event": "hook_invoked",
            "ts_unix_ms": 1_760_000_301_000u64,
            "agent": "cursor",
            "hook_name": "postToolUse",
            "session_id": "analytics-session",
            "tool_name": "Grep",
            "prompt_category": "code_research",
        }),
    ];
    let content = rows
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(
        store_root.join("hook_analytics.jsonl"),
        format!("{content}\n"),
    )
    .expect("write hook analytics");
}

async fn seed_durable_recent_window(runtime: &DashboardTestRuntimeV1, project_root: &Path) {
    let project_id = DashboardTestRuntimeV1::canonical_project_key(project_root);
    let mut events: Vec<_> = (0..10_000)
        .map(|offset| analytics_event(&project_id, 1_760_000_000 + offset, "older_noise"))
        .collect();
    events.push(AnalyticsEventInsert {
        skill_name: Some("superpowers:test-driven-development".to_string()),
        outcome: Some("used".to_string()),
        ..analytics_event(&project_id, 1_760_020_000, "skill")
    });
    runtime
        .append_analytics_events_for_test(HostAdmissionScope::Profile, &events)
        .await
        .expect("append durable analytics events");
}

async fn seed_fallback_analytics(runtime: &DashboardTestRuntimeV1, project_root: &Path) {
    let project_id = DashboardTestRuntimeV1::canonical_project_key(project_root);
    let rows = [
        AnalyticsEventInsert {
            hint_category: Some("search".to_string()),
            hint_id: Some("hint-search".to_string()),
            outcome: Some("observed".to_string()),
            ..analytics_event(&project_id, 1_760_000_200, "hint_emitted")
        },
        AnalyticsEventInsert {
            skill_name: Some("superpowers:test-driven-development".to_string()),
            outcome: Some("used".to_string()),
            ..analytics_event(&project_id, 1_760_000_201, "skill")
        },
        AnalyticsEventInsert {
            tool_name: Some("mcp__tracedecay__tracedecay_context".to_string()),
            tool_category: Some("mcp".to_string()),
            outcome: Some("success".to_string()),
            ..analytics_event("other-project", 1_760_000_202, "mcp_tool_call")
        },
    ];
    for row in rows {
        runtime
            .append_analytics_event_for_test(HostAdmissionScope::Project, &row)
            .await
            .expect("append fallback analytics event");
    }
}

async fn start_fixture(seed_durable_events: bool) -> Fixture {
    let tmp = TempDir::new().expect("temp dir");
    let project_root = tmp.path().join("project");
    std::fs::create_dir_all(&project_root).expect("project dir");
    std::fs::write(
        project_root.join("lib.rs"),
        "pub fn analytics_fixture() {}\n",
    )
    .expect("seed source file");

    let global_db_path = tmp.path().join("global").join("global.db");
    let env_guard = EnvVarGuard::set("TRACEDECAY_GLOBAL_DB", &global_db_path);
    let profile_root = tmp.path().join("profile").join(".tracedecay");
    let data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);
    let project_id =
        tracedecay_domain::ProjectId::new("dashboard_analytics_fixture").expect("project identity");
    let host_runtime = Arc::new(
        DashboardTestRuntimeV1::project(&profile_root, &project_root, project_id)
            .await
            .expect("analytics host-admission runtime"),
    );
    let cg = host_runtime
        .initialize_project_graph_for_test(
            &project_root,
            tracedecay::tracedecay::TraceDecayOpenOptions {
                profile_root: Some(profile_root.clone()),
                global_db_path: Some(global_db_path),
            },
        )
        .await
        .expect("tracedecay init");
    let store_root = cg.store_layout().data_root.clone();
    seed_session_store(&host_runtime, &project_root).await;
    if seed_durable_events {
        seed_durable_analytics(&host_runtime, &project_root).await;
    }

    let port = pick_free_port();
    let base_url = format!("http://127.0.0.1:{port}");
    let server_runtime = Arc::clone(&host_runtime);
    let server_graph = Arc::new(cg);
    let server = tokio::spawn(async move {
        let authority = server_runtime
            .dashboard_test_authority()
            .expect("dashboard analytics authority");
        let _ = dashboard::run_until_shutdown_for_tests_with_host_admission(
            server_graph,
            authority,
            dashboard::DashboardTestProjectGraphsV1::default(),
            "127.0.0.1",
            port,
            dashboard::spa_router(),
            std::future::pending(),
        )
        .await;
    });
    wait_for_dashboard(&http_agent(), &base_url).await;

    Fixture {
        _tmp: tmp,
        _env_guard: env_guard,
        _data_dir_guard: data_dir_guard,
        base_url,
        server,
        project_root,
        store_root,
        host_runtime,
    }
}

fn find_row<'a>(rows: &'a Value, key: &str, value: &str) -> &'a Value {
    rows.as_array()
        .and_then(|items| items.iter().find(|row| row[key] == value))
        .unwrap_or_else(|| panic!("missing row where {key}={value}: {rows:#}"))
}

fn assert_usage_row(usage: &Value, category: &str, events: i64, kind: &str) {
    let row = find_row(usage, "category", category);
    assert_eq!(row["events"], events);
    assert_eq!(row["kind"], kind);
}

fn has_row(rows: &Value, key: &str, value: &str) -> bool {
    rows.as_array()
        .is_some_and(|items| items.iter().any(|row| row[key] == value))
}

#[test]
fn analytics_api_advertises_and_aggregates_session_usage() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_fixture(false).await;
        let agent = http_agent();

        let (status, caps) = get_json(&agent, &format!("{}/api/capabilities", fixture.base_url));
        assert_eq!(status, 200);
        assert_eq!(caps["features"]["analytics"], true);
        assert!(
            caps["dashboards"]
                .as_array()
                .is_some_and(|dashboards| dashboards.iter().all(|name| name != "analytics")),
            "capabilities should not advertise an analytics dashboard until a bundle exists"
        );

        let (status, overview) = get_json(
            &agent,
            &format!("{}/api/plugins/analytics/overview", fixture.base_url),
        );
        assert_eq!(status, 200);
        assert!(overview["db"].as_str().is_some_and(|path| !path.is_empty()));
        assert_eq!(overview["hints"]["available"], false);
        assert_eq!(overview["hints"]["by_category"][0]["emitted"], 0);

        let usage = &overview["usage"]["by_category"];
        assert_usage_row(usage, "tracedecay_mcp", 2, "tool");
        assert_eq!(
            find_row(usage, "category", "broad_code_context")["events"],
            2
        );
        assert_usage_row(usage, "tracedecay_workflow_skill", 1, "skill");

        let agents = &overview["agents"]["by_agent"];
        assert_eq!(find_row(agents, "agent", "code-explorer")["sessions"], 1);
        assert_eq!(
            find_row(agents, "agent", "session-historian")["sessions"],
            1
        );
        assert_eq!(
            find_row(agents, "agent", "code-health-auditor")["sessions"],
            1
        );
        assert!(!has_row(agents, "agent", "worker"));

        let code_context = find_row(
            &overview["underused_tool_families"],
            "family",
            "code_context",
        );
        assert_eq!(code_context["relevant_events"], 1);
        assert_eq!(code_context["usage_events"], 1);
        assert_eq!(code_context["underused"], false);
    });
}

#[test]
fn analytics_api_prefers_durable_events_when_available() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_fixture(true).await;
        let agent = http_agent();

        let (status, overview) = get_json(
            &agent,
            &format!("{}/api/plugins/analytics/overview", fixture.base_url),
        );
        assert_eq!(status, 200);
        assert_eq!(overview["hints"]["source"], "analytics_events");
        assert_eq!(overview["usage"]["source"], "analytics_events");

        let search = find_row(&overview["hints"]["by_category"], "category", "search");
        assert_eq!(search["emitted"], 1);

        let usage = &overview["usage"]["by_category"];
        assert_usage_row(usage, "tracedecay_mcp", 1, "tool");
        assert_usage_row(usage, "workflow_skill", 1, "skill");
    });
}

#[test]
fn analytics_diagnostics_reports_tool_hook_and_prompt_rollups() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_fixture(true).await;
        seed_hook_analytics(&fixture.store_root);
        let agent = http_agent();

        let (status, diagnostics) = get_json(
            &agent,
            &format!("{}/api/plugins/analytics/diagnostics", fixture.base_url),
        );
        assert_eq!(status, 200);
        assert_eq!(diagnostics["source"], "analytics_events");
        assert_eq!(diagnostics["message_count"], 4);
        assert_eq!(diagnostics["event_count"], 3);
        assert_eq!(diagnostics["mcp_tool_call_count"], 1);
        assert_eq!(diagnostics["tracedecay_call_count"], 1);
        assert_eq!(diagnostics["hook_call_count"], 2);
        assert_eq!(diagnostics["ratios"]["mcp_tool_calls_per_message"], 0.25);
        assert_eq!(diagnostics["ratios"]["hook_calls_per_message"], 0.5);

        assert_eq!(
            find_row(&diagnostics["by_tool_category"], "tool_category", "mcp")["count"],
            1
        );
        assert_eq!(
            find_row(&diagnostics["by_hook"], "hook_name", "UserPromptSubmit")["count"],
            1
        );
        assert_eq!(
            find_row(
                &diagnostics["by_prompt_category"],
                "prompt_category",
                "dashboard_or_ui"
            )["count"],
            1
        );
        assert_eq!(
            diagnostics["recent_hooks"][0]["hook_name"], "postToolUse",
            "recent hook rows should be newest-first"
        );
    });
}

#[test]
fn analytics_api_filters_fallback_events_to_current_project() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_fixture(false).await;
        seed_fallback_analytics(&fixture.host_runtime, &fixture.project_root).await;
        let agent = http_agent();

        let (status, overview) = get_json(
            &agent,
            &format!("{}/api/plugins/analytics/overview", fixture.base_url),
        );
        assert_eq!(status, 200);
        assert_eq!(overview["hints"]["source"], "analytics_events");
        assert_eq!(overview["usage"]["source"], "analytics_events");

        let search = find_row(&overview["hints"]["by_category"], "category", "search");
        assert_eq!(search["emitted"], 1);

        let usage = &overview["usage"]["by_category"];
        assert_usage_row(usage, "workflow_skill", 1, "skill");
        assert!(
            !has_row(usage, "category", "tracedecay_mcp"),
            "other-project fallback events must not leak into current project usage: {usage:#}"
        );
    });
}

#[test]
fn analytics_api_uses_recent_durable_events_when_window_is_capped() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_fixture(false).await;
        seed_durable_recent_window(&fixture.host_runtime, &fixture.project_root).await;
        let agent = http_agent();

        let (status, overview) = get_json(
            &agent,
            &format!("{}/api/plugins/analytics/overview", fixture.base_url),
        );
        assert_eq!(status, 200);
        assert_eq!(overview["usage"]["source"], "analytics_events");
        assert_eq!(overview["usage"]["event_count"], 10_000);

        assert_usage_row(
            &overview["usage"]["by_category"],
            "workflow_skill",
            1,
            "skill",
        );
    });
}

#[test]
fn observatory_and_costs_http_dashboard_export_preserve_value_and_coverage() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_fixture(true).await;
        let agent = http_agent();

        let (status, observatory_dashboard) =
            get_json(&agent, &format!("{}/api/observatory", fixture.base_url));
        assert_eq!(status, 200);
        assert_eq!(observatory_dashboard["schema_revision"], 1);
        let (status, observatory_http) = get_json(
            &agent,
            &format!("{}/api/plugins/analytics/observatory", fixture.base_url),
        );
        assert_eq!(status, 200);
        let (status, observatory_export) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/analytics/observatory/export",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(
            metric_parity_view(&observatory_dashboard["payload"]["metrics"]),
            metric_parity_view(&observatory_http["metrics"])
        );
        assert_eq!(
            metric_parity_view(&observatory_http["metrics"]),
            metric_parity_view(&observatory_export["metrics"])
        );

        let (status, costs_dashboard) =
            get_json(&agent, &format!("{}/api/costs", fixture.base_url));
        assert_eq!(status, 200);
        assert_eq!(costs_dashboard["schema_revision"], 1);
        let (status, costs_http) = get_json(
            &agent,
            &format!("{}/api/plugins/savings/costs", fixture.base_url),
        );
        assert_eq!(status, 200);
        let (status, costs_export) = get_json(
            &agent,
            &format!("{}/api/plugins/savings/costs/export", fixture.base_url),
        );
        assert_eq!(status, 200);
        for series in ["usage", "estimated_cost"] {
            assert_eq!(
                metric_parity_view(&costs_dashboard["payload"][series]),
                metric_parity_view(&costs_http[series]),
                "costs {series} drifted between the dashboard and plugin reads"
            );
            assert_eq!(
                metric_parity_view(&costs_http[series]),
                metric_parity_view(&costs_export[series]),
                "costs {series} drifted between the plugin read and its export"
            );
        }
    });
}

/// Metric content with each read's own observation horizon removed.
///
/// Every surface answers a separate request and stamps `temporal.horizon` with
/// the window it actually observed, so two reads taken microseconds apart
/// legitimately carry different `until_micros`. Parity is about value,
/// coverage, provenance, and cohort agreeing across surfaces; the horizon is
/// checked here for being a real observed window instead.
fn metric_parity_view(metrics: &Value) -> Vec<Value> {
    let rows = metrics
        .as_array()
        .unwrap_or_else(|| panic!("expected a metrics array: {metrics}"));
    assert!(
        !rows.is_empty(),
        "metric parity needs at least one metric: {metrics}"
    );
    rows.iter()
        .map(|metric| {
            let horizon = &metric["temporal"]["horizon"];
            let (Some(since), Some(until)) = (
                horizon["since_micros"].as_i64(),
                horizon["until_micros"].as_i64(),
            ) else {
                panic!("metric omitted its observation horizon: {metric}");
            };
            assert!(
                since < until,
                "each read must stamp a real observed window: {horizon}"
            );
            let mut compared = metric.clone();
            compared["temporal"]["horizon"] = Value::Null;
            compared
        })
        .collect()
}

#[test]
fn observatory_counts_canonical_failed_outcomes() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_fixture(false).await;
        let project_id = DashboardTestRuntimeV1::canonical_project_key(&fixture.project_root);
        fixture
            .host_runtime
            .append_analytics_event_for_test(
                HostAdmissionScope::Profile,
                &observability_event(
                    &project_id,
                    tracedecay::tracedecay::current_timestamp(),
                    ObservabilityTerminalResultV1::Failed,
                ),
            )
            .await
            .expect("append canonical failed event");

        let (status, observatory) = get_json(
            &http_agent(),
            &format!("{}/api/observatory", fixture.base_url),
        );
        assert_eq!(status, 200);
        let failures = observatory["payload"]["metrics"]
            .as_array()
            .and_then(|metrics| {
                metrics
                    .iter()
                    .find(|metric| metric["metric"] == "observability_failures")
            })
            .expect("observability failures metric");
        assert_eq!(failures["value"], 1.0);
    });
}

#[test]
fn costs_read_model_is_mounted_on_the_active_dashboard() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_fixture(false).await;
        let (status, costs) = get_json(&http_agent(), &format!("{}/api/costs", fixture.base_url));
        assert_eq!(status, 200);
        assert_eq!(costs["payload"]["authorized_scope_ref"], "all");
        assert!(costs["payload"]["usage"].is_array());
        assert!(costs["payload"]["estimated_cost"].is_array());
    });
}
