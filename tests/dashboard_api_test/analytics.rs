//! Integration tests for dashboard durable analytics endpoints.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use crate::common::{
    create_runtime, get_json, http_agent, message_record_at, pick_free_port, wait_for_dashboard,
    write_empty_global_db_schema, EnvVarGuard, GLOBAL_DB_ENV_LOCK as ENV_LOCK,
};
use serde_json::Value;
use tempfile::TempDir;
use tracedecay::dashboard;
use tracedecay::global_db::{AnalyticsEventInsert, GlobalDb};
use tracedecay::sessions::cursor::project_session_db_path;
use tracedecay::sessions::{SessionMessageRecord, SessionRecord};
use tracedecay::storage::resolve_layout_for_current_profile;
use tracedecay::tracedecay::TraceDecay;

struct Fixture {
    _tmp: TempDir,
    _env_guard: EnvVarGuard,
    base_url: String,
    server: tokio::task::JoinHandle<()>,
    project_root: PathBuf,
    global_db_path: PathBuf,
    session_db_path: PathBuf,
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

#[allow(clippy::too_many_arguments)]
fn message(
    id: &str,
    role: &str,
    ordinal: i64,
    text: &str,
    kind: &str,
    tool_names: Option<&str>,
    metadata_json: Option<&str>,
) -> SessionMessageRecord {
    message_record_at(
        "codex",
        id,
        "analytics-session",
        role,
        ordinal,
        Some(1_760_000_000 + ordinal),
        text,
        kind,
        Some("gpt-5.5"),
        tool_names,
        None,
        None,
        metadata_json,
    )
}

async fn seed_session_store(db_path: &Path, project: &Path) {
    let gdb = GlobalDb::open_at(db_path).await.expect("open session db");
    assert!(gdb.upsert_session(&session(project)).await);

    let rows = [
        message(
            "msg-1",
            "assistant",
            1,
            "Using tracedecay:searching-for-code before shell search.",
            "message",
            Some("mcp__tracedecay__tracedecay_context,mcp__tracedecay__tracedecay_search"),
            Some(r#"{"skills":["tracedecay:searching-for-code"]}"#),
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
        assert!(gdb.upsert_session_message(&row).await);
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

async fn seed_durable_analytics(db_path: &Path, project_root: &Path) {
    let gdb = GlobalDb::open_at(db_path).await.expect("open global db");
    let project_id = GlobalDb::canonical_project_key(project_root);
    let rows = [
        AnalyticsEventInsert {
            hint_category: Some("search".to_string()),
            hint_id: Some("hint-search".to_string()),
            outcome: Some("shown".to_string()),
            ..analytics_event(&project_id, 1_760_000_100, "hint")
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
        gdb.append_analytics_event(&row)
            .await
            .expect("append durable analytics event");
    }
}

fn seed_hook_analytics(project_root: &Path) {
    let layout = resolve_layout_for_current_profile(project_root).expect("resolve store layout");
    std::fs::create_dir_all(&layout.data_root).expect("create store root");
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
        layout.data_root.join("hook_analytics.jsonl"),
        format!("{content}\n"),
    )
    .expect("write hook analytics");
}

async fn seed_durable_recent_window(db_path: &Path, project_root: &Path) {
    let gdb = GlobalDb::open_at(db_path).await.expect("open global db");
    let project_id = GlobalDb::canonical_project_key(project_root);
    let mut events: Vec<_> = (0..10_000)
        .map(|offset| analytics_event(&project_id, 1_760_000_000 + offset, "older_noise"))
        .collect();
    events.push(AnalyticsEventInsert {
        skill_name: Some("superpowers:test-driven-development".to_string()),
        outcome: Some("used".to_string()),
        ..analytics_event(&project_id, 1_760_020_000, "skill")
    });
    gdb.append_analytics_events(&events)
        .await
        .expect("append durable analytics events");
}

async fn seed_fallback_analytics(db_path: &Path, project_root: &Path) {
    let gdb = GlobalDb::open_at(db_path).await.expect("open session db");
    let project_id = GlobalDb::canonical_project_key(project_root);
    let rows = [
        AnalyticsEventInsert {
            hint_category: Some("search".to_string()),
            hint_id: Some("hint-search".to_string()),
            outcome: Some("shown".to_string()),
            ..analytics_event(&project_id, 1_760_000_200, "hint")
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
        gdb.append_analytics_event(&row)
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
    // Pre-create both GlobalDb-schema stores from the cached empty template
    // so seeding and dashboard startup open existing DBs instead of paying a
    // full schema creation each (slow on Windows).
    write_empty_global_db_schema(&global_db_path).await;
    let cg = TraceDecay::init(&project_root)
        .await
        .expect("tracedecay init");
    let session_db_path = project_session_db_path(&project_root);
    write_empty_global_db_schema(&session_db_path).await;
    seed_session_store(&session_db_path, &project_root).await;
    if seed_durable_events {
        seed_durable_analytics(&global_db_path, &project_root).await;
    }

    let port = pick_free_port();
    let base_url = format!("http://127.0.0.1:{port}");
    let server = tokio::spawn(async move {
        let _ = dashboard::run(&cg, "127.0.0.1", port, false).await;
    });
    wait_for_dashboard(&http_agent(), &base_url).await;

    Fixture {
        _tmp: tmp,
        _env_guard: env_guard,
        base_url,
        server,
        project_root,
        global_db_path,
        session_db_path,
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
        assert_eq!(
            overview["db"],
            fixture.session_db_path.display().to_string()
        );
        assert_eq!(overview["hints"]["available"], false);
        assert_eq!(overview["hints"]["by_category"][0]["emitted"], 0);

        let usage = &overview["usage"]["by_category"];
        assert_usage_row(usage, "tracedecay_mcp", 2, "tool");
        assert_eq!(
            find_row(usage, "category", "broad_code_context")["events"],
            2
        );
        assert_usage_row(usage, "tracedecay_workflow_skill", 1, "skill");

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
        seed_hook_analytics(&fixture.project_root);
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
        seed_fallback_analytics(&fixture.session_db_path, &fixture.project_root).await;
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
        seed_durable_recent_window(&fixture.global_db_path, &fixture.project_root).await;
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
