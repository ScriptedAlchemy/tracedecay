//! Integration tests for the Savings & Cost dashboard API
//! (`/api/plugins/savings/*`), against a seeded temp global DB for accounting
//! and the resolved project session store for transcript cost rows.
//!
//! Pricing uses the deterministic bundled all-provider snapshot.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use crate::common::{
    EnvVarGuard, GLOBAL_DB_ENV_LOCK as ENV_LOCK, create_runtime, get_json, http_agent,
    pick_free_port, wait_for_dashboard,
};
use crate::dashboard_api_support::{MessageDetails, MessageRecordBuilder, message};
use crate::runtime::DashboardTestRuntimeV1;
use serde_json::Value;
use std::sync::Arc;
use tempfile::TempDir;
use tracedecay::config::USER_DATA_DIR_ENV;
use tracedecay::dashboard;
use tracedecay::global_db::ParseOffset;
use tracedecay_sessions::runtime::SessionRecord;
use tracedecay_usecases::host_admission::HostAdmissionScope;

struct Fixture {
    _tmp: TempDir,
    _env_guards: Vec<EnvVarGuard>,
    base_url: String,
    server: tokio::task::JoinHandle<()>,
    /// Start of the current UTC day; seeded timestamps hang off this.
    day_start: i64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FixtureSeed {
    Base,
    LedgerOnly,
    DailyLimitRegression,
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

/// Chars/4 estimate matching the backend SQL `(LENGTH(text)+3)/4`.
fn est_tokens(text: &str) -> i64 {
    (text.len() as i64 + 3) / 4
}

const TEXT_USER: &str = "Please add a savings and cost accounting tab to the dashboard.";
const TEXT_ASSISTANT: &str =
    "Done: the new tab reads the savings ledger and prices sessions with OpenRouter data.";
const TEXT_UNKNOWN: &str = "This message was stored without any model id attached.";
const TEXT_MIXED: &str = "Second message of the mixed session, no usage record here.";

struct SavingsSeed<'a>(&'a DashboardTestRuntimeV1);

impl SavingsSeed<'_> {
    async fn upsert(&self, project: &Path, tokens_saved: u64) {
        self.0.upsert(project, tokens_saved).await;
    }

    async fn record_savings(
        &self,
        project: &str,
        tool: &str,
        before: u64,
        after: u64,
        timestamp: i64,
    ) {
        self.0
            .record_savings_for_test(project, tool, before, after, timestamp)
            .await;
    }

    async fn upsert_session(&self, session: &SessionRecord) -> bool {
        self.0
            .upsert_session_for_test(HostAdmissionScope::Project, session)
            .await
            .expect("seed savings session")
    }

    async fn upsert_session_message(
        &self,
        message: &tracedecay_sessions::runtime::SessionMessageRecord,
    ) -> bool {
        self.0
            .upsert_session_message_for_test(HostAdmissionScope::Project, message)
            .await
            .expect("seed savings session message")
    }

    async fn upsert_transcript_batch(
        &self,
        session: &SessionRecord,
        messages: &[tracedecay_sessions::runtime::SessionMessageRecord],
        source: &str,
        offset: ParseOffset,
    ) -> bool {
        self.0
            .upsert_transcript_batch_for_test(
                HostAdmissionScope::Project,
                session,
                messages,
                source,
                offset,
            )
            .await
            .is_ok()
    }
}

async fn seed_ledger_db(runtime: &DashboardTestRuntimeV1, project: &Path, day_start: i64) {
    let gdb = SavingsSeed(runtime);

    // Lifetime counter (legacy `projects.tokens_saved`, what `tracedecay
    // gain` reports as the lifetime number).
    gdb.upsert(project, 47_000).await;

    // Savings ledger: two events today, one yesterday (same shape as
    // tests/gain_test.rs so totals line up with the CLI behavior).
    gdb.record_savings("/proj/a", "tracedecay_context", 10_000, 500, day_start + 10)
        .await;
    gdb.record_savings("/proj/b", "tracedecay_context", 5_000, 250, day_start + 20)
        .await;
    gdb.record_savings(
        "/proj/a",
        "tracedecay_search",
        2_000,
        100,
        day_start - 86_390,
    )
    .await;
}

async fn seed_global_db(runtime: &DashboardTestRuntimeV1, project: &Path, day_start: i64) {
    seed_ledger_db(runtime, project, day_start).await;
    let gdb = SavingsSeed(runtime);

    // S1: transcript metadata carries Anthropic usage fields. The costs
    // projection must ignore them because only admitted provider observations
    // are authoritative for billing.
    assert!(
        gdb.upsert_session(&session(
            "sess-usage",
            project,
            day_start + 100,
            "Usage-backed session"
        ))
        .await
    );
    assert!(
        gdb.upsert_session_message(&message(
            "m-usage-1",
            "sess-usage",
            "assistant",
            1,
            TEXT_ASSISTANT,
            MessageDetails {
                timestamp: day_start + 120,
                model: Some("claude-fable-5-thinking-high"),
                metadata_json: Some(
                    r#"{"usage":{"input_tokens":1200,"output_tokens":350,"cache_read_input_tokens":9000,"cache_creation_input_tokens":50}}"#
                ),
            },
        ))
        .await
    );

    // S2: no usage anywhere → estimated (chars/4, user→input, assistant→output).
    assert!(
        gdb.upsert_session(&session(
            "sess-estimated",
            project,
            day_start + 200,
            "Estimated session"
        ))
        .await
    );
    assert!(
        gdb.upsert_session_message(&message(
            "m-est-1",
            "sess-estimated",
            "user",
            1,
            TEXT_USER,
            MessageDetails {
                timestamp: day_start + 210,
                model: Some("gpt-5.5-high"),
                metadata_json: None,
            },
        ))
        .await
    );
    assert!(
        gdb.upsert_session_message(&message(
            "m-est-2",
            "sess-estimated",
            "assistant",
            2,
            TEXT_ASSISTANT,
            MessageDetails {
                timestamp: day_start + 220,
                model: Some("gpt-5.5-high"),
                metadata_json: None,
            },
        ))
        .await
    );

    // S3: no model id recorded at all → "unknown model" row, never priced.
    assert!(
        gdb.upsert_session(&session(
            "sess-unknown",
            project,
            day_start + 300,
            "Unknown-model session"
        ))
        .await
    );
    assert!(
        gdb.upsert_session_message(&message(
            "m-unknown-1",
            "sess-unknown",
            "assistant",
            1,
            TEXT_UNKNOWN,
            MessageDetails {
                timestamp: day_start + 310,
                model: None,
                metadata_json: None,
            },
        ))
        .await
    );

    // S4: usage (OpenAI field names) + a usage-less message → mixed.
    assert!(
        gdb.upsert_session(&session(
            "sess-mixed",
            project,
            day_start + 400,
            "Mixed session"
        ))
        .await
    );
    assert!(
        gdb.upsert_session_message(&message(
            "m-mixed-1",
            "sess-mixed",
            "assistant",
            1,
            TEXT_ASSISTANT,
            MessageDetails {
                timestamp: day_start + 410,
                model: Some("claude-opus-4-8-thinking-max"),
                metadata_json: Some(r#"{"usage":{"prompt_tokens":500,"completion_tokens":700}}"#,),
            },
        ))
        .await
    );
    assert!(
        gdb.upsert_session_message(&message(
            "m-mixed-2",
            "sess-mixed",
            "assistant",
            2,
            TEXT_MIXED,
            MessageDetails {
                timestamp: day_start + 420,
                model: Some("claude-opus-4-8-thinking-max"),
                metadata_json: None,
            },
        ))
        .await
    );

    // S5: transcript metadata carries the shape Codex backfill writes. It is
    // still content metadata, not provider-usage billing authority.
    assert!(
        gdb.upsert_session(&session(
            "sess-codex",
            project,
            day_start + 500,
            "Codex usage-backed session"
        ))
        .await
    );
    assert!(
        gdb.upsert_session_message(&message(
            "m-codex-1",
            "sess-codex",
            "assistant",
            1,
            TEXT_ASSISTANT,
            MessageDetails {
                timestamp: day_start + 510,
                model: Some("gpt-5.3-codex-high"),
                metadata_json: Some(
                    r#"{"usage":{"input_tokens":900,"output_tokens":150,"cache_read_input_tokens":4000,"total_tokens":5050}}"#
                ),
            },
        ))
        .await
    );
    assert!(
        gdb.upsert_session_message(
            &MessageRecordBuilder::new(
                "cursor",
                "m-codex-summary",
                "sess-codex",
                "assistant",
                2,
                "Synthetic Codex compaction placeholder that is not real model output.",
                "summary",
            )
            .with_timestamp(Some(day_start + 520))
            .with_model(Some("gpt-5.3-codex-high"))
            .build()
        )
        .await
    );
}

async fn seed_daily_limit_regression(
    runtime: &DashboardTestRuntimeV1,
    project: &Path,
    latest_day: i64,
) {
    let gdb = SavingsSeed(runtime);

    let daily_session = session(
        "sess-daily-limit",
        project,
        latest_day + 30,
        "Daily limit regression",
    );
    let mut messages = Vec::new();
    for offset in 0..=366 {
        let timestamp = latest_day - (offset * 86_400) + 60;
        messages.push(message(
            &format!("m-daily-limit-{offset}"),
            "sess-daily-limit",
            "assistant",
            offset,
            "Daily limit accounting row.",
            MessageDetails {
                timestamp,
                model: Some("daily-limit-a"),
                metadata_json: Some(r#"{"usage":{"input_tokens":1,"output_tokens":1}}"#),
            },
        ));
    }

    messages.push(message(
        "m-daily-limit-latest-b",
        "sess-daily-limit",
        "assistant",
        500,
        "Second latest-day model bucket.",
        MessageDetails {
            timestamp: latest_day + 90,
            model: Some("daily-limit-b"),
            metadata_json: Some(r#"{"usage":{"input_tokens":1,"output_tokens":1}}"#),
        },
    ));

    assert!(
        gdb.upsert_transcript_batch(
            &daily_session,
            &messages,
            "daily-limit-regression.jsonl",
            ParseOffset::default(),
        )
        .await
    );
}

async fn start_fixture(seed: FixtureSeed) -> Fixture {
    let tmp = TempDir::new().expect("temp dir");
    let project_root = tmp.path().join("project");
    std::fs::create_dir_all(&project_root).expect("project dir");
    std::fs::write(
        project_root.join("lib.rs"),
        "pub fn savings_fixture() -> u32 { 7 }\n",
    )
    .expect("seed source file");

    let global_db_path = tmp.path().join("global").join("global.db");
    let profile_root = tmp.path().join("profile").join(".tracedecay");
    let env_guards = vec![
        EnvVarGuard::set("TRACEDECAY_GLOBAL_DB", &global_db_path),
        EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root),
        // `.cargo/config.toml` disables global accounting for cargo-launched
        // processes; opt back in so the recording state reads "enabled".
        EnvVarGuard::set("TRACEDECAY_ENABLE_GLOBAL_DB", "1"),
    ];

    let now = now_unix();
    let day_start = now - (now % 86_400);

    let project_id =
        tracedecay_domain::ProjectId::new("dashboard_savings_fixture").expect("project identity");
    let host_runtime = Arc::new(
        DashboardTestRuntimeV1::project(&profile_root, &project_root, project_id)
            .await
            .expect("savings host-admission runtime"),
    );
    let cg = host_runtime
        .initialize_project_graph_for_test(
            &project_root,
            tracedecay::tracedecay::TraceDecayOpenOptions {
                profile_root: Some(profile_root.clone()),
                global_db_path: None,
            },
        )
        .await
        .expect("tracedecay init");
    match seed {
        FixtureSeed::Base => seed_global_db(&host_runtime, &project_root, day_start).await,
        FixtureSeed::LedgerOnly => seed_ledger_db(&host_runtime, &project_root, day_start).await,
        FixtureSeed::DailyLimitRegression => {
            seed_daily_limit_regression(&host_runtime, &project_root, day_start).await;
        }
    }
    let port = pick_free_port();
    let base_url = format!("http://127.0.0.1:{port}");
    let server_runtime = Arc::clone(&host_runtime);
    let server_graph = Arc::new(cg);
    let server = tokio::spawn(async move {
        let authority = server_runtime
            .dashboard_test_authority()
            .expect("dashboard savings authority");
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
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_fixture(FixtureSeed::LedgerOnly).await;
        let agent = http_agent();

        // Capability flag + tab registration.
        let (status, caps) = get_json(&agent, &format!("{}/api/capabilities", fixture.base_url));
        assert_eq!(status, 200);
        assert_eq!(caps["features"]["savings"], true);
        assert_eq!(caps["dashboards"], serde_json::json!(["tracedecay"]));

        // Overview: ledger totals + lifetime counters.
        let (status, overview) = get_json(
            &agent,
            &format!("{}/api/plugins/savings/overview", fixture.base_url),
        );
        assert_eq!(status, 200);
        assert_eq!(overview["schema_revision"], 1);
        let overview = &overview["payload"];
        let savings = &overview["savings"];
        assert_eq!(savings["available"], true);
        // The dashboard surfaces the ledger-recording gate state so an
        // empty ledger is explained honestly instead of "no events yet".
        assert_eq!(savings["recording"]["enabled"], true);
        assert_eq!(savings["recording"]["mode"], "enabled_by_env");
        assert_eq!(savings["ledger"]["all_time"]["saved_tokens"], 16_150);
        assert_eq!(savings["ledger"]["all_time"]["calls"], 3);
        assert_eq!(savings["ledger"]["today"]["saved_tokens"], 14_250);
        assert_eq!(savings["ledger"]["today"]["calls"], 2);
        assert_eq!(savings["lifetime_counters"]["total_tokens_saved"], 47_000);
        assert_eq!(savings["lifetime_counters"]["project_total"], 1);
        assert_eq!(savings["lifetime_counters"]["projects_limit"], 25);
        assert_eq!(savings["lifetime_counters"]["projects_truncated"], false);
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
            .find(|row| row["tool"] == "tracedecay_context")
            .expect("context tool row");
        assert_eq!(context["saved_tokens"], 14_250);
        assert_eq!(context["calls"], 2);
        let search = by_tool
            .iter()
            .find(|row| row["tool"] == "tracedecay_search")
            .expect("search tool row");
        assert_eq!(search["saved_tokens"], 1_900);
        let by_project = ledger["by_project"].as_array().expect("by_project");
        assert_eq!(by_project.len(), 2);
        assert!(
            by_project
                .iter()
                .any(|row| row["project"] == "/proj/a" && row["saved_tokens"] == 11_400)
        );
        let by_day = ledger["by_day"].as_array().expect("by_day");
        assert_eq!(by_day.len(), 2, "today + yesterday buckets");

        // Range filter narrows to today's events.
        let (_, today) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/savings/ledger?range=today",
                fixture.base_url
            ),
        );
        assert_eq!(today["total"]["saved_tokens"], 14_250);
        assert_eq!(today["total"]["calls"], 2);
    });
}

#[test]
fn daily_model_series_limits_days_not_model_rows() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_fixture(FixtureSeed::DailyLimitRegression).await;

        let (_, models) = get_json(
            &http_agent(),
            &format!("{}/api/plugins/savings/models?range=all", fixture.base_url),
        );
        let daily = models["daily"].as_array().expect("daily rows");
        let oldest_included_day = fixture.day_start - (365 * 86_400);
        let excluded_day = fixture.day_start - (366 * 86_400);

        assert!(
            daily
                .iter()
                .any(|row| row["day"] == fixture.day_start && row["model"] == "daily-limit-a"),
            "latest day/model row was truncated: {daily:?}"
        );
        assert!(
            daily
                .iter()
                .any(|row| row["day"] == fixture.day_start && row["model"] == "daily-limit-b"),
            "second latest-day model row was truncated: {daily:?}"
        );
        assert!(
            daily.iter().any(|row| row["day"] == oldest_included_day),
            "expected the 366th latest day to remain: {daily:?}"
        );
        assert!(
            daily.iter().all(|row| row["day"] != excluded_day),
            "row limit included an older day outside the 366-day window: {daily:?}"
        );

        assert_eq!(models["provider_usage"]["available"], false);
    });
}

#[test]
fn session_content_counts_ignore_metadata_usage_without_canonical_provider_evidence() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_fixture(FixtureSeed::Base).await;
        let agent = http_agent();

        // Whether this build carries the BPE tokenizer (the `token-counting`
        // feature, on by default). Non-usage messages land in the
        // "tokenized" tier with it, in the chars/4 "estimated" tier without.
        let (_, overview) = get_json(
            &agent,
            &format!("{}/api/plugins/savings/overview", fixture.base_url),
        );
        let overview = &overview["payload"];
        let counting = overview["sessions"]["token_counting"] == true;
        let nonusage_basis = if counting { "tokenized" } else { "estimated" };

        let (status, payload) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/savings/sessions?range=all",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(payload["available"], true);
        assert_eq!(payload["total"], 5);

        // Session metadata is content context, never billing authority.
        let usage_session = find_session(&payload, "sess-usage");
        assert_eq!(usage_session["cost_basis"], nonusage_basis);
        assert_eq!(usage_session["provider_usage_events"], 0);
        let usage_model = &usage_session["models"][0];
        assert_eq!(usage_model["model"], "claude-fable-5-thinking-high");
        assert_eq!(usage_model["cost_basis"], nonusage_basis);
        assert!(usage_model["provider_actual"].is_null());

        // S2: no usage → tokenized (BPE-counted) when the tokenizer is
        // compiled in, chars/4 estimated otherwise. gpt-5.5-high maps to
        // the o200k_base encoder exactly.
        let nonusage_session = find_session(&payload, "sess-estimated");
        assert_eq!(nonusage_session["cost_basis"], nonusage_basis);
        let nonusage_model = &nonusage_session["models"][0];
        assert_eq!(nonusage_model["model"], "gpt-5.5-high");
        assert_eq!(nonusage_model["cost_basis"], nonusage_basis);
        assert!(nonusage_model["provider_actual"].is_null());
        if counting {
            assert_eq!(nonusage_model["tokenizer"]["encoder"], "o200k_base");
            assert_eq!(nonusage_model["tokenizer"]["exact"], true);
            assert_eq!(nonusage_model["tokenized_messages"], 2);
            assert_eq!(nonusage_model["estimated_messages"], 0);
            let bpe_in = nonusage_model["tokenized"]["input_tokens"]
                .as_i64()
                .expect("tokenized input");
            let bpe_out = nonusage_model["tokenized"]["output_tokens"]
                .as_i64()
                .expect("tokenized output");
            assert!(bpe_in > 0 && bpe_in <= TEXT_USER.len() as i64);
            assert!(bpe_out > 0 && bpe_out <= TEXT_ASSISTANT.len() as i64);
            assert_eq!(nonusage_model["estimated"]["input_tokens"], 0);
            assert_eq!(nonusage_model["estimated"]["output_tokens"], 0);
        } else {
            assert_eq!(
                nonusage_model["estimated"]["input_tokens"],
                est_tokens(TEXT_USER)
            );
            assert_eq!(
                nonusage_model["estimated"]["output_tokens"],
                est_tokens(TEXT_ASSISTANT)
            );
        }

        // S3: no model id → null model, tokens still counted (approximate
        // o200k when tokenized — there is no tokenizer to be exact with).
        let unknown_session = find_session(&payload, "sess-unknown");
        let unknown_model = &unknown_session["models"][0];
        assert!(unknown_model["model"].is_null());
        if counting {
            assert_eq!(unknown_model["tokenizer"]["exact"], false);
            assert!(unknown_model["tokenized"]["output_tokens"].as_i64() > Some(0));
        } else {
            assert_eq!(
                unknown_model["estimated"]["output_tokens"],
                est_tokens(TEXT_UNKNOWN)
            );
        }

        // Codex-shaped metadata is ignored for the same reason.
        let codex_session = find_session(&payload, "sess-codex");
        assert_eq!(codex_session["cost_basis"], nonusage_basis);
        let codex_model = &codex_session["models"][0];
        assert_eq!(codex_model["model"], "gpt-5.3-codex-high");
        assert_eq!(codex_model["cost_basis"], nonusage_basis);
        assert!(codex_model["provider_actual"].is_null());

        // Mixed metadata/no-metadata rows remain one content-count tier.
        let mixed_session = find_session(&payload, "sess-mixed");
        assert_eq!(mixed_session["cost_basis"], nonusage_basis);
        let mixed_model = &mixed_session["models"][0];
        assert_eq!(mixed_model["cost_basis"], nonusage_basis);
        assert!(mixed_model["provider_actual"].is_null());
        if counting {
            // claude-* has no public tokenizer → labeled approximation.
            assert_eq!(mixed_model["tokenizer"]["exact"], false);
            assert!(mixed_model["tokenized"]["output_tokens"].as_i64() > Some(0));
        } else {
            assert_eq!(
                mixed_model["estimated"]["output_tokens"],
                est_tokens(TEXT_MIXED)
            );
        }

        // Models endpoint: per-model content aggregates and canonical provider usage.
        let (_, models) = get_json(
            &agent,
            &format!("{}/api/plugins/savings/models?range=all", fixture.base_url),
        );
        let fable = find_model(
            &models["models"],
            &Value::String("claude-fable-5-thinking-high".into()),
        );
        assert_eq!(fable["cost_basis"], nonusage_basis);
        assert_eq!(fable["sessions"], 1);
        let unknown = find_model(&models["models"], &Value::Null);
        assert_eq!(unknown["cost_basis"], nonusage_basis);

        let usage_by_model = models["provider_usage"]["by_model"]
            .as_array()
            .expect("provider usage");
        assert!(usage_by_model.is_empty());

        let daily = models["daily"].as_array().expect("daily");
        assert_eq!(
            daily.len(),
            5,
            "daily session costs should keep one row per day/model price bucket"
        );
        assert!(
            daily.iter().all(|row| row["day"] == fixture.day_start),
            "all seeded daily rows should stay in the same UTC day"
        );
        assert!(
            daily.iter().any(|row| row["model"] == "gpt-5.5-high"),
            "daily rows must carry model ids so frontend price lookup works"
        );
        assert!(
            daily.iter().any(|row| row["model"].is_null()),
            "unknown-model daily rows should remain explicit"
        );
        let usage_by_day = models["provider_usage"]["by_day"]
            .as_array()
            .expect("provider usage by day");
        assert!(usage_by_day.is_empty());

        // Overview session stats roll the same seven content messages up.
        let sessions = &overview["sessions"];
        assert_eq!(sessions["available"], true);
        assert_eq!(sessions["session_count"], 5);
        assert_eq!(sessions["messages"], 7);
        // No observation projection checkpoint exists in this fixture, so the
        // canonical usage-event count is unknowable and stays null — the same
        // typed state `provider_usage.available == false` below encodes. A
        // fabricated 0 here would claim a measured absence that was never
        // observed.
        assert!(
            sessions["provider_usage_events"].is_null(),
            "canonical usage-event count must stay unknown without a projection checkpoint: {sessions}"
        );
        if counting {
            assert_eq!(sessions["tokenized_messages"], 7);
            assert_eq!(sessions["estimated_messages"], 0);
        } else {
            assert_eq!(sessions["tokenized_messages"], 0);
            assert_eq!(sessions["estimated_messages"], 7);
        }
        assert_eq!(sessions["unknown_model_messages"], 1);
        assert_eq!(sessions["model_count"], 4);
        assert_eq!(sessions["cost_basis"], nonusage_basis);
        // The provider-usage aggregate resolves the exact project scope but
        // no observation projection checkpoint exists, so the read is typed
        // unavailable: zero deltas were scanned (a real count over the scan,
        // not a claimed measurement) and no token totals are fabricated.
        assert_eq!(overview["provider_usage"]["available"], false);
        assert_eq!(overview["provider_usage"]["status"], "unavailable");
        assert_eq!(overview["provider_usage"]["usage_event_count"], 0);
        assert!(overview["provider_usage"]["total_tokens"].is_null());
    });
}

#[test]
fn pricing_serves_content_addressed_bundled_snapshot() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_fixture(FixtureSeed::Base).await;
        let agent = http_agent();

        let (status, pricing) = get_json(
            &agent,
            &format!("{}/api/plugins/savings/pricing", fixture.base_url),
        );
        assert_eq!(status, 200);
        assert_eq!(pricing["source"], "bundled");
        assert_eq!(pricing["offline"], true);
        assert!(pricing["fetched_at"].is_null());
        assert!(
            pricing["model_count"].as_i64().expect("model count") > 50,
            "bundled snapshot should carry a broad model set"
        );
        let fable = &pricing["models"]["anthropic/claude-fable-5"];
        assert!(fable["prompt_per_mtok"].as_f64().expect("prompt price") > 0.0);
        assert!(
            fable["completion_per_mtok"]
                .as_f64()
                .expect("completion price")
                > 0.0
        );

        // The overview embeds the same provenance block.
        let (_, overview) = get_json(
            &agent,
            &format!("{}/api/plugins/savings/overview", fixture.base_url),
        );
        assert_eq!(overview["payload"]["pricing"]["source"], "bundled");
        assert_eq!(overview["payload"]["pricing"]["offline"], true);
    });
}
