use std::io::Write;

use tempfile::TempDir;
use tokensave::sessions::codex::CodexSource;
use tokensave::sessions::cursor::open_project_session_db;
use tokensave::sessions::source::ingest_source;

fn setup(tmp: &TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir(project.join(".tokensave")).unwrap();
    std::fs::write(project.join(".tokensave/tokensave.db"), "").unwrap();
    (home, project)
}

/// Writes a Codex rollout JSONL whose `session_meta.cwd` is `project`. Includes a
/// `response_item` line that must be ignored (it duplicates the agent_message).
fn write_codex_rollout(
    home: &std::path::Path,
    project: &std::path::Path,
    session: &str,
) -> std::path::PathBuf {
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("rollout-2026-01-01T00-00-00-{session}.jsonl"));
    let contents = format!(
        "{}\n{}\n{}\n{}\n{}\n",
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:00.000Z",
            "type": "session_meta",
            "payload": {"id": session, "cwd": project.to_string_lossy(), "model": "gpt-5.5"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "Investigate the billing pipeline regression"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:02.000Z",
            "type": "event_msg",
            "payload": {
                "type": "agent_message",
                "message": "The billing pipeline regression is fixed.",
                "tool_calls": [
                    {
                        "id": "call-1",
                        "function": {
                            "name": "apply_patch",
                            "arguments": {"path": "src/lib.rs"}
                        }
                    }
                ]
            }
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:02.500Z",
            "type": "response_item",
            "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "duplicate"}]}
        }),
        // Per-turn usage arrives as a separate token_count event after the
        // agent_message (real rollout shape, OpenAI semantics).
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:02.600Z",
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {
                    "total_token_usage": {"input_tokens": 14662, "cached_input_tokens": 6528, "output_tokens": 13, "reasoning_output_tokens": 0, "total_tokens": 14675},
                    "last_token_usage": {"input_tokens": 14662, "cached_input_tokens": 6528, "output_tokens": 13, "reasoning_output_tokens": 0, "total_tokens": 14675},
                    "model_context_window": 258400
                }
            }
        }),
    );
    std::fs::write(&path, contents).unwrap();
    path
}

fn write_codex_subagent_rollout(
    home: &std::path::Path,
    project: &std::path::Path,
    parent_session: &str,
    child_session: &str,
) -> std::path::PathBuf {
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("rollout-2026-01-01T00-00-10-{child_session}.jsonl"));
    let contents = format!(
        "{}\n{}\n",
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:10.000Z",
            "type": "session_meta",
            "payload": {
                "id": child_session,
                "cwd": project.to_string_lossy(),
                "model_provider": "openai",
                "thread_source": "subagent",
                "forked_from_id": parent_session,
                "agent_nickname": "Euler",
                "agent_role": "explorer",
                "source": {
                    "subagent": {
                        "thread_spawn": {
                            "parent_thread_id": parent_session,
                            "agent_nickname": "Euler",
                            "agent_role": "explorer",
                            "depth": 1
                        }
                    }
                }
            }
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:11.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "The child worker verified Codex layout evidence."}
        }),
    );
    std::fs::write(&path, contents).unwrap();
    path
}

#[tokio::test]
async fn codex_rollout_populates_user_and_agent_messages_only() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_codex_rollout(&home, &project, "codex-sess");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);

    let stats = ingest_source(&db, &source, &project, None).await;
    // user_message + agent_message; the response_item duplicate is skipped.
    assert_eq!(stats.messages_upserted, 2);
    assert_eq!(stats.sessions_upserted, 1);

    let results = db
        .search_session_messages(
            "codex",
            Some(project.to_string_lossy().as_ref()),
            "billing pipeline",
            10,
        )
        .await;
    assert_eq!(results.len(), 2);
    assert!(results.iter().any(|hit| hit.message.role == "user"));
    assert!(results.iter().any(|hit| hit.message.role == "assistant"));
    assert!(results
        .iter()
        .all(|hit| hit.message.model.as_deref() == Some("gpt-5.5")));
    // Rollout ISO-8601 timestamps land as epoch seconds (2026-01-01).
    assert!(results
        .iter()
        .any(|hit| hit.message.timestamp == Some(1_767_225_601)));
    assert!(results
        .iter()
        .any(|hit| hit.message.timestamp == Some(1_767_225_602)));
    let assistant = results
        .iter()
        .find(|hit| hit.message.role == "assistant")
        .expect("assistant message should be searchable");
    assert_eq!(assistant.message.tool_names.as_deref(), Some("apply_patch"));
    let raw = db
        .lcm_load_raw_message("codex", &assistant.message.message_id)
        .await
        .expect("Codex tool_calls should be in raw LCM metadata");
    let metadata: serde_json::Value =
        serde_json::from_str(raw.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["tool_calls"][0]["function"]["name"], "apply_patch");

    // The trailing token_count event's per-turn usage attaches to the
    // assistant reply it reports on, normalized for the savings dashboard's
    // additive pricing: input excludes the cached portion (OpenAI input
    // includes it), which lands in cache_read_input_tokens.
    assert_eq!(metadata["usage"]["input_tokens"], 14662 - 6528);
    assert_eq!(metadata["usage"]["cache_read_input_tokens"], 6528);
    assert_eq!(metadata["usage"]["output_tokens"], 13);
    assert_eq!(metadata["usage"]["total_tokens"], 14675);
    let user = results
        .iter()
        .find(|hit| hit.message.role == "user")
        .expect("user message should be searchable");
    let user_metadata: serde_json::Value =
        serde_json::from_str(user.message.metadata_json.as_deref().unwrap()).unwrap();
    assert!(user_metadata.get("usage").is_none());
}

#[tokio::test]
async fn codex_rollout_ingest_is_incremental() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let path = write_codex_rollout(&home, &project, "codex-sess");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);

    assert_eq!(
        ingest_source(&db, &source, &project, None)
            .await
            .messages_upserted,
        2
    );
    assert_eq!(
        ingest_source(&db, &source, &project, None)
            .await
            .messages_upserted,
        0
    );

    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(
        f,
        "{}",
        serde_json::json!({
            "timestamp": "2026-01-01T00:01:00.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "Added a regression test."}
        })
    )
    .unwrap();
    drop(f);

    assert_eq!(
        ingest_source(&db, &source, &project, None)
            .await
            .messages_upserted,
        1
    );
}

/// Archived rollouts (`~/.codex/archived_sessions/rollout-*.jsonl`, flat
/// layout) are real transcripts and must be swept like live ones. The real
/// machine had 22 of them invisible to ingestion before this fix.
#[tokio::test]
async fn codex_archived_rollout_is_ingested() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    // Native joins keep the expected path separator-identical to the stored
    // transcript_path on Windows.
    let dir = home.join(".codex").join("archived_sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rollout-2026-01-01T00-00-00-archived-sess.jsonl");
    let contents = format!(
        "{}\n{}\n",
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:00.000Z",
            "type": "session_meta",
            "payload": {"id": "archived-sess", "cwd": project.to_string_lossy()}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "Archived rollout probe"}
        }),
    );
    std::fs::write(&path, contents).unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);

    let stats = ingest_source(&db, &source, &project, None).await;
    assert_eq!(stats.sessions_upserted, 1);
    assert_eq!(stats.messages_upserted, 1);
    let session = db
        .get_session("codex", "archived-sess")
        .await
        .expect("archived rollout session should be stored");
    assert_eq!(
        session.transcript_path.as_deref(),
        Some(path.to_string_lossy().as_ref())
    );
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
    ingest_source(&db, &source, &project, None).await;

    let usage_of = |hits: &[tokensave::sessions::SessionMessageSearchResult], needle: &str| {
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
    ingest_source(&db, &source, &project, None).await;

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
async fn codex_subagent_rollout_uses_parent_link_from_session_meta() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_codex_rollout(&home, &project, "codex-parent");
    write_codex_subagent_rollout(&home, &project, "codex-parent", "codex-child");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);

    let stats = ingest_source(&db, &source, &project, None).await;
    assert_eq!(stats.sessions_upserted, 2);
    assert_eq!(stats.messages_upserted, 3);

    let child = db
        .get_session("codex", "codex-child")
        .await
        .expect("subagent session should be stored");
    assert_eq!(child.parent_session_id.as_deref(), Some("codex-parent"));
    assert!(child.is_subagent);
    assert_eq!(child.agent_id.as_deref(), Some("codex-child"));

    let results = db
        .search_session_messages("codex", None, "layout evidence", 10)
        .await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].session.session_id, "codex-child");
}
