//! Integration tests for the Savings & Cost dashboard API
//! (`/api/plugins/savings/*`), against a seeded temp global DB serving both
//! the savings ledger and the session store (`TOKENSAVE_GLOBAL_DB` override).
//!
//! Pricing runs offline (`TOKENSAVE_OFFLINE=1`) with the cache pointed at a
//! nonexistent temp path, so the bundled fallback snapshot is exercised.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::net::TcpListener;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::Value;
use tempfile::TempDir;
use tokensave::dashboard;
use tokensave::global_db::GlobalDb;
use tokensave::sessions::{SessionMessageRecord, SessionRecord};
use tokensave::tokensave::TokenSave;
use tokensave::types::CostTurn;

/// Serializes tests in this binary: they mutate process-wide env vars.
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            std::env::set_var(self.key, previous);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

struct Fixture {
    _tmp: TempDir,
    _env_guards: Vec<EnvVarGuard>,
    base_url: String,
    server: tokio::task::JoinHandle<()>,
    /// Start of the current UTC day; seeded timestamps hang off this.
    day_start: i64,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before epoch")
        .as_secs() as i64
}

fn create_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime")
}

fn pick_free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind free port");
    listener.local_addr().expect("local addr").port()
}

fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(4)))
        .build()
        .into()
}

fn get_json(agent: &ureq::Agent, url: &str) -> (u16, Value) {
    let mut response = agent.get(url).call().expect("http call");
    let status = response.status().as_u16();
    let body = response.body_mut().read_to_string().expect("body");
    let parsed = serde_json::from_str::<Value>(&body)
        .unwrap_or_else(|err| panic!("bad JSON body `{body}`: {err}"));
    (status, parsed)
}

fn session(session_id: &str, project: &Path, started_at: i64, title: &str) -> SessionRecord {
    SessionRecord {
        provider: "cursor".to_string(),
        session_id: session_id.to_string(),
        project_key: "savings-fixture".to_string(),
        project_path: project.display().to_string(),
        title: Some(title.to_string()),
        started_at: Some(started_at),
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
    message_id: &str,
    session_id: &str,
    role: &str,
    ordinal: i64,
    timestamp: i64,
    text: &str,
    model: Option<&str>,
    metadata_json: Option<&str>,
) -> SessionMessageRecord {
    SessionMessageRecord {
        provider: "cursor".to_string(),
        message_id: message_id.to_string(),
        session_id: session_id.to_string(),
        role: role.to_string(),
        timestamp: Some(timestamp),
        ordinal,
        text: text.to_string(),
        kind: Some("message".to_string()),
        model: model.map(ToString::to_string),
        tool_names: None,
        source_path: None,
        source_offset: None,
        metadata_json: metadata_json.map(ToString::to_string),
    }
}

/// Chars/4 estimate matching the backend SQL `(LENGTH(text)+3)/4`.
fn est_tokens(text: &str) -> i64 {
    (text.len() as i64 + 3) / 4
}

const TEXT_USER: &str = "Please add a savings and cost accounting tab to the dashboard.";
const TEXT_ASSISTANT: &str =
    "Done: the new tab reads the savings ledger and prices sessions with OpenRouter data.";
const TEXT_UNKNOWN: &str = "This message was stored without any model id attached.";
const TEXT_MIXED: &str = "Second message of the mixed session, no usage record here.";

async fn seed_global_db(db_path: &Path, project: &Path, day_start: i64) {
    let gdb = GlobalDb::open_at(db_path).await.expect("open global db");

    // Lifetime counter (legacy `projects.tokens_saved`, what `tokensave
    // gain` reports as the lifetime number).
    gdb.upsert(project, 47_000).await;

    // Savings ledger: two events today, one yesterday (same shape as
    // tests/gain_test.rs so totals line up with the CLI behavior).
    gdb.record_savings("/proj/a", "tokensave_context", 10_000, 500, day_start + 10)
        .await;
    gdb.record_savings("/proj/b", "tokensave_context", 5_000, 250, day_start + 20)
        .await;
    gdb.record_savings("/proj/a", "tokensave_search", 2_000, 100, day_start - 86_390)
        .await;

    // Claude Code accounting turn (cost computed from real usage at ingest).
    assert!(
        gdb.insert_turn(&CostTurn {
            message_id: "turn-1".to_string(),
            project_hash: "fixture".to_string(),
            session_id: "claude-sess".to_string(),
            model: "claude-opus-4-6".to_string(),
            timestamp: (day_start + 50) as u64,
            input_tokens: 100_000,
            output_tokens: 20_000,
            cache_write_tokens: 0,
            cache_read_tokens: 0,
            cost_usd: 1.25,
            category: "code".to_string(),
            tool_names: String::new(),
        })
        .await
    );

    // S1: every message has transcript usage (Anthropic field names) → actual.
    assert!(gdb.upsert_session(&session("sess-usage", project, day_start + 100, "Usage-backed session")).await);
    assert!(
        gdb.upsert_session_message(&message(
            "m-usage-1",
            "sess-usage",
            "assistant",
            1,
            day_start + 120,
            TEXT_ASSISTANT,
            Some("claude-fable-5-thinking-high"),
            Some(
                r#"{"usage":{"input_tokens":1200,"output_tokens":350,"cache_read_input_tokens":9000,"cache_creation_input_tokens":50}}"#
            ),
        ))
        .await
    );

    // S2: no usage anywhere → estimated (chars/4, user→input, assistant→output).
    assert!(gdb.upsert_session(&session("sess-estimated", project, day_start + 200, "Estimated session")).await);
    assert!(
        gdb.upsert_session_message(&message(
            "m-est-1",
            "sess-estimated",
            "user",
            1,
            day_start + 210,
            TEXT_USER,
            Some("gpt-5.5-high"),
            None,
        ))
        .await
    );
    assert!(
        gdb.upsert_session_message(&message(
            "m-est-2",
            "sess-estimated",
            "assistant",
            2,
            day_start + 220,
            TEXT_ASSISTANT,
            Some("gpt-5.5-high"),
            None,
        ))
        .await
    );

    // S3: no model id recorded at all → "unknown model" row, never priced.
    assert!(gdb.upsert_session(&session("sess-unknown", project, day_start + 300, "Unknown-model session")).await);
    assert!(
        gdb.upsert_session_message(&message(
            "m-unknown-1",
            "sess-unknown",
            "assistant",
            1,
            day_start + 310,
            TEXT_UNKNOWN,
            None,
            None,
        ))
        .await
    );

    // S4: usage (OpenAI field names) + a usage-less message → mixed.
    assert!(gdb.upsert_session(&session("sess-mixed", project, day_start + 400, "Mixed session")).await);
    assert!(
        gdb.upsert_session_message(&message(
            "m-mixed-1",
            "sess-mixed",
            "assistant",
            1,
            day_start + 410,
            TEXT_ASSISTANT,
            Some("claude-opus-4-8-thinking-max"),
            Some(r#"{"usage":{"prompt_tokens":500,"completion_tokens":700}}"#),
        ))
        .await
    );
    assert!(
        gdb.upsert_session_message(&message(
            "m-mixed-2",
            "sess-mixed",
            "assistant",
            2,
            day_start + 420,
            TEXT_MIXED,
            Some("claude-opus-4-8-thinking-max"),
            None,
        ))
        .await
    );
}

async fn start_fixture() -> Fixture {
    let tmp = TempDir::new().expect("temp dir");
    let project_root = tmp.path().join("project");
    std::fs::create_dir_all(&project_root).expect("project dir");
    std::fs::write(
        project_root.join("lib.rs"),
        "pub fn savings_fixture() -> u32 { 7 }\n",
    )
    .expect("seed source file");

    let global_db_path = tmp.path().join("global").join("global.db");
    let env_guards = vec![
        EnvVarGuard::set("TOKENSAVE_GLOBAL_DB", &global_db_path.display().to_string()),
        EnvVarGuard::set("TOKENSAVE_OFFLINE", "1"),
        // Point the pricing cache at a path that never exists → the bundled
        // fallback snapshot must serve.
        EnvVarGuard::set(
            "TOKENSAVE_MODEL_PRICES_PATH",
            &tmp.path().join("no-such-prices.json").display().to_string(),
        ),
    ];

    let now = now_unix();
    let day_start = now - (now % 86_400);
    seed_global_db(&global_db_path, &project_root, day_start).await;

    let cg = TokenSave::init(&project_root).await.expect("tokensave init");
    let port = pick_free_port();
    let base_url = format!("http://127.0.0.1:{port}");
    let server = tokio::spawn(async move {
        let _ = dashboard::run(&cg, "127.0.0.1", port, false).await;
    });

    let agent = http_agent();
    let probe = format!("{base_url}/api/capabilities");
    for _ in 0..80 {
        if agent.get(&probe).call().is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    Fixture {
        _tmp: tmp,
        _env_guards: env_guards,
        base_url,
        server,
        day_start,
    }
}

fn find_session<'a>(payload: &'a Value, session_id: &str) -> &'a Value {
    payload["sessions"]
        .as_array()
        .expect("sessions array")
        .iter()
        .find(|row| row["session_id"] == session_id)
        .unwrap_or_else(|| panic!("session {session_id} missing from payload"))
}

fn find_model<'a>(rows: &'a Value, model: &Value) -> &'a Value {
    rows.as_array()
        .expect("model rows array")
        .iter()
        .find(|row| &row["model"] == model)
        .unwrap_or_else(|| panic!("model row {model} missing"))
}

#[test]
fn savings_ledger_endpoints_reflect_seeded_ledger() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_fixture().await;
        let agent = http_agent();

        // Capability flag + tab registration.
        let (status, caps) = get_json(&agent, &format!("{}/api/capabilities", fixture.base_url));
        assert_eq!(status, 200);
        assert_eq!(caps["features"]["savings"], true);
        assert!(caps["dashboards"]
            .as_array()
            .expect("dashboards")
            .iter()
            .any(|name| name == "savings"));
        let (_, plugins) = get_json(
            &agent,
            &format!("{}/api/dashboard/plugins", fixture.base_url),
        );
        assert!(plugins
            .as_array()
            .expect("plugins")
            .iter()
            .any(|plugin| plugin["name"] == "savings"));

        // Overview: ledger totals + lifetime counters.
        let (status, overview) =
            get_json(&agent, &format!("{}/api/plugins/savings/overview", fixture.base_url));
        assert_eq!(status, 200);
        let savings = &overview["savings"];
        assert_eq!(savings["available"], true);
        assert_eq!(savings["ledger"]["all_time"]["saved_tokens"], 16_150);
        assert_eq!(savings["ledger"]["all_time"]["calls"], 3);
        assert_eq!(savings["ledger"]["today"]["saved_tokens"], 14_250);
        assert_eq!(savings["ledger"]["today"]["calls"], 2);
        assert_eq!(savings["lifetime_counters"]["total_tokens_saved"], 47_000);
        assert_eq!(
            savings["lifetime_counters"]["projects"]
                .as_array()
                .expect("projects")
                .len(),
            1
        );

        // Ledger breakdowns (range=all).
        let (_, ledger) = get_json(
            &agent,
            &format!("{}/api/plugins/savings/ledger?range=all", fixture.base_url),
        );
        assert_eq!(ledger["total"]["saved_tokens"], 16_150);
        let by_tool = ledger["by_tool"].as_array().expect("by_tool");
        let context = by_tool
            .iter()
            .find(|row| row["tool"] == "tokensave_context")
            .expect("context tool row");
        assert_eq!(context["saved_tokens"], 14_250);
        assert_eq!(context["calls"], 2);
        let search = by_tool
            .iter()
            .find(|row| row["tool"] == "tokensave_search")
            .expect("search tool row");
        assert_eq!(search["saved_tokens"], 1_900);
        let by_project = ledger["by_project"].as_array().expect("by_project");
        assert_eq!(by_project.len(), 2);
        assert!(by_project
            .iter()
            .any(|row| row["project"] == "/proj/a" && row["saved_tokens"] == 11_400));
        let by_day = ledger["by_day"].as_array().expect("by_day");
        assert_eq!(by_day.len(), 2, "today + yesterday buckets");

        // Range filter narrows to today's events.
        let (_, today) = get_json(
            &agent,
            &format!("{}/api/plugins/savings/ledger?range=today", fixture.base_url),
        );
        assert_eq!(today["total"]["saved_tokens"], 14_250);
        assert_eq!(today["total"]["calls"], 2);
    });
}

#[test]
fn session_costs_label_actual_vs_estimated() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_fixture().await;
        let agent = http_agent();

        let (status, payload) = get_json(
            &agent,
            &format!("{}/api/plugins/savings/sessions?range=all", fixture.base_url),
        );
        assert_eq!(status, 200);
        assert_eq!(payload["available"], true);
        assert_eq!(payload["total"], 4);

        // S1: usage-backed → actual, with exact usage token counts.
        let usage_session = find_session(&payload, "sess-usage");
        assert_eq!(usage_session["cost_basis"], "actual");
        assert_eq!(usage_session["usage_messages"], 1);
        assert_eq!(usage_session["estimated_messages"], 0);
        let usage_model = &usage_session["models"][0];
        assert_eq!(usage_model["model"], "claude-fable-5-thinking-high");
        assert_eq!(usage_model["cost_basis"], "actual");
        assert_eq!(usage_model["actual"]["input_tokens"], 1_200);
        assert_eq!(usage_model["actual"]["output_tokens"], 350);
        assert_eq!(usage_model["actual"]["cache_read_tokens"], 9_000);
        assert_eq!(usage_model["actual"]["cache_write_tokens"], 50);
        assert_eq!(usage_model["estimated"]["input_tokens"], 0);
        assert_eq!(usage_model["estimated"]["output_tokens"], 0);

        // S2: no usage → estimated, chars/4 with role attribution.
        let estimated_session = find_session(&payload, "sess-estimated");
        assert_eq!(estimated_session["cost_basis"], "estimated");
        let estimated_model = &estimated_session["models"][0];
        assert_eq!(estimated_model["model"], "gpt-5.5-high");
        assert_eq!(estimated_model["cost_basis"], "estimated");
        assert_eq!(
            estimated_model["estimated"]["input_tokens"],
            est_tokens(TEXT_USER)
        );
        assert_eq!(
            estimated_model["estimated"]["output_tokens"],
            est_tokens(TEXT_ASSISTANT)
        );
        assert_eq!(estimated_model["actual"]["input_tokens"], 0);

        // S3: no model id → null model, tokens still counted.
        let unknown_session = find_session(&payload, "sess-unknown");
        let unknown_model = &unknown_session["models"][0];
        assert!(unknown_model["model"].is_null());
        assert_eq!(
            unknown_model["estimated"]["output_tokens"],
            est_tokens(TEXT_UNKNOWN)
        );

        // S4: usage + non-usage on one model → mixed, OpenAI usage keys read.
        let mixed_session = find_session(&payload, "sess-mixed");
        assert_eq!(mixed_session["cost_basis"], "mixed");
        let mixed_model = &mixed_session["models"][0];
        assert_eq!(mixed_model["cost_basis"], "mixed");
        assert_eq!(mixed_model["actual"]["input_tokens"], 500);
        assert_eq!(mixed_model["actual"]["output_tokens"], 700);
        assert_eq!(
            mixed_model["estimated"]["output_tokens"],
            est_tokens(TEXT_MIXED)
        );

        // Models endpoint: per-model aggregates + turns accounting + daily.
        let (_, models) = get_json(
            &agent,
            &format!("{}/api/plugins/savings/models?range=all", fixture.base_url),
        );
        let fable = find_model(
            &models["models"],
            &Value::String("claude-fable-5-thinking-high".into()),
        );
        assert_eq!(fable["cost_basis"], "actual");
        assert_eq!(fable["sessions"], 1);
        let unknown = find_model(&models["models"], &Value::Null);
        assert_eq!(unknown["cost_basis"], "estimated");

        let turns_by_model = models["turns"]["by_model"].as_array().expect("turns");
        assert_eq!(turns_by_model.len(), 1);
        assert_eq!(turns_by_model[0]["model"], "claude-opus-4-6");
        assert_eq!(turns_by_model[0]["cost_basis"], "actual");
        assert!((turns_by_model[0]["cost_usd"].as_f64().expect("cost") - 1.25).abs() < 1e-9);

        let daily = models["daily"].as_array().expect("daily");
        assert_eq!(daily.len(), 1, "all seeded messages share one UTC day");
        assert_eq!(daily[0]["day"], fixture.day_start);
        let turns_by_day = models["turns"]["by_day"].as_array().expect("turns by day");
        assert_eq!(turns_by_day.len(), 1);

        // Overview session stats roll the same numbers up.
        let (_, overview) =
            get_json(&agent, &format!("{}/api/plugins/savings/overview", fixture.base_url));
        let sessions = &overview["sessions"];
        assert_eq!(sessions["session_count"], 4);
        assert_eq!(sessions["messages"], 6);
        assert_eq!(sessions["usage_messages"], 2);
        assert_eq!(sessions["estimated_messages"], 4);
        assert_eq!(sessions["unknown_model_messages"], 1);
        assert_eq!(sessions["model_count"], 3);
        assert_eq!(sessions["cost_basis"], "mixed");
    });
}

#[test]
fn pricing_serves_bundled_fallback_when_offline() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_fixture().await;
        let agent = http_agent();

        let (status, pricing) = get_json(
            &agent,
            &format!("{}/api/plugins/savings/pricing", fixture.base_url),
        );
        assert_eq!(status, 200);
        assert_eq!(pricing["source"], "fallback");
        assert_eq!(pricing["offline"], true);
        assert!(pricing["fetched_at"].is_null());
        assert!(
            pricing["model_count"].as_i64().expect("model count") > 50,
            "bundled snapshot should carry a broad model set"
        );
        let fable = &pricing["models"]["anthropic/claude-fable-5"];
        assert!(fable["prompt_per_mtok"].as_f64().expect("prompt price") > 0.0);
        assert!(fable["completion_per_mtok"].as_f64().expect("completion price") > 0.0);

        // The overview embeds the same provenance block.
        let (_, overview) =
            get_json(&agent, &format!("{}/api/plugins/savings/overview", fixture.base_url));
        assert_eq!(overview["pricing"]["source"], "fallback");
        assert_eq!(overview["pricing"]["offline"], true);

        // Embedded frontend assets serve for the new plugin.
        let asset = agent
            .get(format!(
                "{}/dashboard-plugins/savings/dist/index.js",
                fixture.base_url
            ))
            .call()
            .expect("asset fetch");
        assert_eq!(asset.status().as_u16(), 200);
    });
}
