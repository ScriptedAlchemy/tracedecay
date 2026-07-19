//! Codex usage and telemetry: token counters, per-turn usage summation, model
//! tracking from turn context, and the structured-event row mix.

use tempfile::TempDir;
use tracedecay::sessions::codex::CodexSource;
use tracedecay::sessions::cursor::open_project_session_db;
use tracedecay::sessions::source::try_ingest_source;

use crate::codex::write_codex_rollout_with_structured_events;
use crate::support::setup;

/// Search this project's Codex messages, then keep only rows of the requested
/// kind (row text is not always unique to one kind, so filter after the query).
async fn search_session_kind(
    db: &tracedecay::global_db::GlobalDb,
    scope: &str,
    query: &str,
    kind: &str,
) -> Vec<tracedecay::sessions::SessionMessageRecord> {
    db.search_session_messages("codex", Some(scope), query, 50)
        .await
        .into_iter()
        .map(|hit| hit.message)
        .filter(|message| message.kind.as_deref() == Some(kind))
        .collect()
}

/// Real session_meta lines carry only `model_provider` ("openai"), which is
/// not a model; the active model lives on `turn_context` lines and can change
/// mid-session. Messages must carry the model active when they were emitted.
#[tokio::test]
async fn codex_model_tracks_turn_context_not_model_provider() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rollout-2026-01-01T00-00-00-model-sess.jsonl");
    let cwd = project.to_string_lossy();
    let lines = [
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:00.000Z",
            "type": "session_meta",
            "payload": {"id": "model-sess", "cwd": cwd, "model_provider": "openai"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:00.500Z",
            "type": "turn_context",
            "payload": {"turn_id": "t1", "cwd": cwd, "model": "gpt-5.3-codex"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "Probe model alpha"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:02.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "Reply from model alpha"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:03.000Z",
            "type": "turn_context",
            "payload": {"turn_id": "t2", "cwd": cwd, "model": "gpt-5.5"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:04.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "Reply from model beta"}
        }),
    ];
    let contents = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&path, contents).unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();

    let hits = db.search_session_messages("codex", None, "model", 10).await;
    assert_eq!(hits.len(), 3);
    let model_of = |needle: &str| {
        hits.iter()
            .find(|hit| hit.message.text.contains(needle))
            .unwrap_or_else(|| panic!("message containing {needle:?} should exist"))
            .message
            .model
            .clone()
    };
    assert_eq!(
        model_of("Probe model alpha").as_deref(),
        Some("gpt-5.3-codex")
    );
    assert_eq!(
        model_of("Reply from model alpha").as_deref(),
        Some("gpt-5.3-codex")
    );
    assert_eq!(
        model_of("Reply from model beta").as_deref(),
        Some("gpt-5.5")
    );
}
#[tokio::test]
async fn codex_usage_preserves_cache_only_total_only_and_reasoning_counters() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rollout-2026-01-01T00-00-40-usage-edge.jsonl");
    let cwd = project.to_string_lossy();
    let lines = [
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:40.000Z",
            "type": "session_meta",
            "payload": {"id": "usage-edge", "cwd": cwd}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:41.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "Usage edge prompt one"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:42.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "Usage edge reply one"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:43.000Z",
            "type": "event_msg",
            "payload": {"type": "token_count", "info": {
                "last_token_usage": {"cache_read_input_tokens": 123, "total_tokens": 123}
            }}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:44.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "Usage edge prompt two"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:45.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "Usage edge reply two"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:46.000Z",
            "type": "event_msg",
            "payload": {"type": "token_count", "info": {
                "last_token_usage": {
                    "input_tokens": 10,
                    "output_tokens": 5,
                    "reasoning_output_tokens": 7,
                    "total_tokens": 22
                }
            }}
        }),
    ];
    std::fs::write(
        &path,
        lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();

    let hits = db
        .search_session_messages("codex", None, "Usage edge reply", 10)
        .await;
    let usage_of = |needle: &str| {
        let hit = hits
            .iter()
            .find(|hit| hit.message.text.contains(needle))
            .unwrap_or_else(|| panic!("message containing {needle:?} should exist"));
        let metadata: serde_json::Value =
            serde_json::from_str(hit.message.metadata_json.as_deref().unwrap()).unwrap();
        metadata["usage"].clone()
    };

    let cache_only = usage_of("reply one");
    assert_eq!(cache_only["input_tokens"], 0);
    assert_eq!(cache_only["output_tokens"], 0);
    assert_eq!(cache_only["cache_read_input_tokens"], 123);
    assert_eq!(cache_only["total_tokens"], 123);

    let reasoning = usage_of("reply two");
    assert_eq!(reasoning["input_tokens"], 10);
    assert_eq!(reasoning["output_tokens"], 12);
    assert_eq!(reasoning["reasoning_tokens"], 7);
    assert_eq!(reasoning["total_tokens"], 22);
}

/// A turn's tool loop emits one `token_count` per API call (most *before* the
/// final agent_message); the turn's true cost is the sum. Real rollouts showed
/// ~64% of input spend in those mid-turn reports. Duplicate reports (cumulative
/// total did not advance) must not double-count, and one turn's calls must not
/// leak into another turn's reply.
#[tokio::test]
async fn codex_tool_loop_usage_sums_per_turn_and_skips_duplicates() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rollout-2026-01-01T00-00-00-loop-sess.jsonl");
    let cwd = project.to_string_lossy();
    let tc = |input: i64, cached: i64, output: i64, total: i64, cumulative: i64| {
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:02.000Z",
            "type": "event_msg",
            "payload": {"type": "token_count", "info": {
                "total_token_usage": {"total_tokens": cumulative},
                "last_token_usage": {
                    "input_tokens": input,
                    "cached_input_tokens": cached,
                    "output_tokens": output,
                    "total_tokens": total
                }
            }}
        })
    };
    let lines = vec![
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:00.000Z",
            "type": "session_meta",
            "payload": {"id": "loop-sess", "cwd": cwd}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "First turn prompt"}
        }),
        // Tool-loop call 1 reports BEFORE the reply; then a duplicate report
        // of the same call (cumulative total unchanged) that must be skipped.
        tc(1000, 600, 50, 1050, 1050),
        tc(1000, 600, 50, 1050, 1050),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:03.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "First turn reply"}
        }),
        // Final call of turn 1 reports after the reply.
        tc(2000, 1500, 100, 2100, 3150),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:04.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "Second turn prompt"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:05.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "Second turn reply"}
        }),
        tc(3000, 0, 10, 3010, 6160),
    ];
    let contents = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&path, contents).unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();

    let usage_of = |hits: &[tracedecay::sessions::SessionMessageSearchResult], needle: &str| {
        let hit = hits
            .iter()
            .find(|hit| hit.message.text.contains(needle))
            .unwrap_or_else(|| panic!("message containing {needle:?} should exist"));
        let metadata: serde_json::Value =
            serde_json::from_str(hit.message.metadata_json.as_deref().unwrap()).unwrap();
        metadata["usage"].clone()
    };
    let hits = db.search_session_messages("codex", None, "reply", 10).await;
    assert_eq!(hits.len(), 2);

    // Turn 1 = call1 + final call (duplicate skipped): uncached input
    // 400 + 500, cached 600 + 1500, output 50 + 100, total 1050 + 2100.
    let first = usage_of(&hits, "First turn reply");
    assert_eq!(first["input_tokens"], 900);
    assert_eq!(first["cache_read_input_tokens"], 2100);
    assert_eq!(first["output_tokens"], 150);
    assert_eq!(first["total_tokens"], 3150);

    // Turn 2 stands alone; no cache_read key when nothing was cached.
    let second = usage_of(&hits, "Second turn reply");
    assert_eq!(second["input_tokens"], 3000);
    assert_eq!(second["output_tokens"], 10);
    assert_eq!(second["total_tokens"], 3010);
    assert!(second.get("cache_read_input_tokens").is_none());
}

#[tokio::test]
async fn codex_structured_events_produce_full_row_mix() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_codex_rollout_with_structured_events(&home, &project, "codex-structured");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    // user + task_started + exec tool_call(joined) + plan + file_edit + mcp
    // tool_call + web_search + sub_agent_activity + inter_agent(edge) +
    // task_complete + agent_message.
    assert_eq!(stats.messages_upserted, 11);

    let scope = project.to_string_lossy().to_string();
    let meta_of = |m: &tracedecay::sessions::SessionMessageRecord| -> serde_json::Value {
        serde_json::from_str(m.metadata_json.as_deref().unwrap()).unwrap()
    };
    let search_kind =
        |query: &'static str, kind: &'static str| search_session_kind(&db, &scope, query, kind);

    // exec_command joined tool_call: exit code / wall time / success parsed.
    let execs = search_kind("cargo nextest quarkonium", "tool_call").await;
    assert_eq!(execs.len(), 1);
    let exec_md = meta_of(&execs[0]);
    assert_eq!(execs[0].tool_names.as_deref(), Some("exec_command"));
    assert_eq!(exec_md["exit_code"], 0);
    assert_eq!(exec_md["wall_time_s"], 2.5);
    assert_eq!(exec_md["success"], true);
    assert_eq!(exec_md["cmd"], "cargo nextest run quarkonium");

    // MCP tool call makes TraceDecay's own adoption visible in Codex sessions.
    let mcp = search_kind("tracedecay context", "tool_call").await;
    let mcp: Vec<_> = mcp
        .into_iter()
        .filter(|m| m.tool_names.as_deref() == Some("tracedecay:tracedecay_context"))
        .collect();
    assert_eq!(mcp.len(), 1);
    let mcp_md = meta_of(&mcp[0]);
    assert_eq!(mcp_md["server"], "tracedecay");
    assert_eq!(mcp_md["ok"], true);
    assert_eq!(mcp_md["duration_ms"], 1500);

    // Plan, file_edit, web_search, turn boundaries, sub-agent activity present.
    assert_eq!(search_kind("sweep telemetry ship", "plan").await.len(), 1);
    let file_edit = search_kind("quarkonium.rs Updated", "file_edit").await;
    assert_eq!(file_edit.len(), 1);
    assert_eq!(meta_of(&file_edit[0])["files"][0]["change_type"], "update");
    assert_eq!(search_kind("decay width", "web_search").await.len(), 1);
    // task_started + task_complete both render "Codex turn …".
    assert_eq!(search_kind("Codex turn", "turn_boundary").await.len(), 2);

    // sub_agent_activity + inter_agent routing edge both map to subagent_activity.
    let subagent = search_kind("telemetry_worker", "subagent_activity").await;
    assert_eq!(subagent.len(), 2);
    // The encrypted inter-agent ciphertext is never stored anywhere.
    assert!(subagent.iter().all(|m| {
        !m.metadata_json
            .as_deref()
            .unwrap()
            .contains("gAAAAquarksecret")
    }));
    let edge = subagent
        .iter()
        .find(|m| meta_of(m)["source_event"] == "inter_agent_communication")
        .expect("inter-agent edge row exists");
    assert_eq!(meta_of(edge)["encrypted"], true);

    // Session summary carries policy/effort posture, distinct models, the model
    // context window, and the latest rate-limit snapshot.
    let session = db.get_session("codex", "codex-structured").await.unwrap();
    let sm: serde_json::Value =
        serde_json::from_str(session.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(sm["codex_approval_policy"], "never");
    assert_eq!(sm["codex_sandbox_policy"], "danger-full-access");
    assert_eq!(sm["codex_effort"], "high");
    assert_eq!(sm["codex_model_context_window"], 258_400);
    assert_eq!(sm["codex_rate_limits"]["primary"]["used_percent"], 11.0);
    assert_eq!(sm["codex_rate_limits"]["plan_type"], "pro");

    // Re-ingest is idempotent: every structured row is keyed by message_id.
    let again = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(again.messages_upserted, 0);
}
