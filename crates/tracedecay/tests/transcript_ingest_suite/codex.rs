use std::io::Write;

use tempfile::TempDir;
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_sessions::admission::HostAdmissionScope;
use tracedecay_sessions::runtime::SessionProvider;
use tracedecay_sessions::runtime::shared::durable_project_path_key;

use crate::restart_atomicity::{
    ingest_global_sources_for_provider, ingest_user_provider, open_project_session_db,
    run_user_provider_pass,
};

pub(crate) fn write_jsonl(path: &std::path::Path, lines: &[serde_json::Value]) {
    std::fs::write(
        path,
        lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
}
#[tokio::test]
async fn user_scope_ingests_only_codex_sessions_outside_registered_projects() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let registered = tmp.path().join("registered");
    let general = tmp.path().join("general-chat");
    std::fs::create_dir_all(&registered).unwrap();
    std::fs::create_dir_all(&general).unwrap();
    write_codex_rollout(&home, &registered, "project-session");
    write_codex_rollout(&home, &general, "user-session");
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path().join("profile"))
        .await
        .unwrap();
    ingest_user_provider(&runtime, &home, SessionProvider::Codex, vec![registered]).await;

    assert!(
        runtime
            .session_for_test(HostAdmissionScope::Profile, "codex", "user-session")
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        runtime
            .session_for_test(HostAdmissionScope::Profile, "codex", "project-session")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn user_scope_excludes_codex_turns_after_switching_to_registered_project() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let registered = tmp.path().join("registered");
    let general = tmp.path().join("general-chat");
    std::fs::create_dir_all(&registered).unwrap();
    std::fs::create_dir_all(&general).unwrap();
    let path = write_codex_rollout(&home, &general, "mixed-session");
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:03.000Z",
            "type": "turn_context",
            "payload": {"cwd": registered.to_string_lossy()}
        })
    )
    .unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:04.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "registered project secret"}
        })
    )
    .unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path().join("profile"))
        .await
        .unwrap();
    ingest_user_provider(&runtime, &home, SessionProvider::Codex, vec![registered]).await;

    let search = |query: &'static str| {
        runtime.search_session_messages_for_test(
            HostAdmissionScope::Profile,
            "codex",
            None,
            query,
            10,
        )
    };
    assert_eq!(search("billing pipeline").await.unwrap().len(), 2);
    assert!(
        search("registered project secret")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn project_scopes_split_codex_turns_when_cwd_changes() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project_a = tmp.path().join("project-a");
    let project_b = tmp.path().join("project-b");
    std::fs::create_dir_all(&project_a).unwrap();
    std::fs::create_dir_all(&project_b).unwrap();
    let path = write_codex_rollout(&home, &project_a, "cross-project-session");
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:03.000Z",
            "type": "turn_context",
            "payload": {"cwd": project_b.to_string_lossy()}
        })
    )
    .unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:04.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "project beta private marker"}
        })
    )
    .unwrap();

    let db_a = open_project_session_db(&project_a).await.unwrap();
    ingest_global_sources_for_provider(&home, &db_a, &project_a, Some(SessionProvider::Codex))
        .await;

    assert_eq!(
        db_a.search_session_messages("codex", None, "billing pipeline", 10)
            .await
            .len(),
        2
    );
    assert!(
        db_a.search_session_messages("codex", None, "project beta private marker", 10)
            .await
            .is_empty()
    );
    drop(db_a);

    let db_b = open_project_session_db(&project_b).await.unwrap();
    ingest_global_sources_for_provider(&home, &db_b, &project_b, Some(SessionProvider::Codex))
        .await;
    assert!(
        db_b.search_session_messages("codex", None, "billing pipeline", 10)
            .await
            .is_empty()
    );
    assert_eq!(
        db_b.search_session_messages("codex", None, "project beta private marker", 10)
            .await
            .len(),
        1
    );
}

/// The `session_meta` model is the rollout's starting model; `turn_context`
/// records replace it for the turns that follow, and the session row records
/// which rollout file the projection was read from.
#[tokio::test]
async fn codex_rollout_projects_turn_context_model_and_transcript_path() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = crate::support::setup(&tmp);
    let path = write_codex_rollout(&home, &project, "codex-model-session");
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:03.000Z",
            "type": "turn_context",
            "payload": {"turn_id": "turn-2", "cwd": project.to_string_lossy(), "model": "gpt-5.6"}
        })
    )
    .unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:04.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "billing pipeline follow-up verified"}
        })
    )
    .unwrap();
    drop(file);

    let db = open_project_session_db(&project).await.unwrap();
    ingest_global_sources_for_provider(&home, &db, &project, Some(SessionProvider::Codex)).await;

    let session = db
        .get_session("codex", "codex-model-session")
        .await
        .expect("codex session should be stored");
    crate::support::assert_path_text_eq(
        session
            .transcript_path
            .as_deref()
            .expect("session transcript path"),
        &path,
    );

    let hits = db
        .search_session_messages("codex", None, "billing pipeline", 10)
        .await;
    let model_for = |needle: &str| {
        hits.iter()
            .find(|hit| hit.message.text.contains(needle))
            .unwrap_or_else(|| panic!("{needle}: message should be searchable"))
            .message
            .model
            .clone()
    };
    assert_eq!(
        model_for("The billing pipeline regression is fixed."),
        Some("gpt-5.5".to_owned()),
        "session_meta model is the starting model"
    );
    assert_eq!(
        model_for("billing pipeline follow-up verified"),
        Some("gpt-5.6".to_owned()),
        "turn_context replaces the model for the turns that follow"
    );
}

#[tokio::test]
async fn user_scope_ingests_codex_turns_after_leaving_a_registered_project() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let registered = tmp.path().join("registered");
    let general = tmp.path().join("general-chat");
    std::fs::create_dir_all(&registered).unwrap();
    std::fs::create_dir_all(&general).unwrap();
    let path = write_codex_rollout(&home, &registered, "project-to-user-session");
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:03.000Z",
            "type": "turn_context",
            "payload": {"cwd": general.to_string_lossy()}
        })
    )
    .unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:04.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "general chat private marker"}
        })
    )
    .unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path().join("profile"))
        .await
        .unwrap();
    ingest_user_provider(&runtime, &home, SessionProvider::Codex, vec![registered]).await;

    assert!(
        runtime
            .search_session_messages_for_test(
                HostAdmissionScope::Profile,
                "codex",
                None,
                "billing pipeline",
                10,
            )
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        runtime
            .search_session_messages_for_test(
                HostAdmissionScope::Profile,
                "codex",
                None,
                "general chat private marker",
                10,
            )
            .await
            .unwrap()
            .len(),
        1
    );
}

/// Writes a Codex rollout JSONL whose `session_meta.cwd` is `project`. Includes
/// the `event_msg` assistant reply and the `response_item` message that records
/// the same turn.
pub(crate) fn write_codex_rollout(
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
            "payload": crate::common::codex_user_message_payload("Investigate the billing pipeline regression")
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

/// A rollout carrying the full spread of structured Codex telemetry: a turn
/// boundary pair, a joined `exec_command` tool call, a plan update, a patch
/// application, an MCP tool call, a web search, sub-agent activity, and an
/// encrypted inter-agent routing edge, plus a `turn_context` and a
/// `token_count` with rate limits feeding the session summary.
pub(crate) fn write_codex_rollout_with_structured_events(
    home: &std::path::Path,
    project: &std::path::Path,
    session: &str,
) -> std::path::PathBuf {
    let dir = home.join(".codex/sessions/2026/01/03");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("rollout-2026-01-03T00-00-00-{session}.jsonl"));
    let workdir = project.to_string_lossy().to_string();
    write_jsonl(
        &path,
        &[
            serde_json::json!({
                "timestamp": "2026-01-03T00:00:00.000Z",
                "type": "session_meta",
                "payload": {"id": session, "cwd": workdir, "model_provider": "openai"}
            }),
            serde_json::json!({
                "timestamp": "2026-01-03T00:00:00.500Z",
                "type": "turn_context",
                "payload": {
                    "turn_id": "turn-1", "cwd": workdir, "model": "gpt-5.5",
                    "approval_policy": "never",
                    "sandbox_policy": {"type": "danger-full-access"},
                    "effort": "high"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-01-03T00:00:01.000Z",
                "type": "event_msg",
                "payload": crate::common::codex_user_message_payload("quarkonium telemetry sweep")
            }),
            serde_json::json!({
                "timestamp": "2026-01-03T00:00:01.100Z",
                "type": "event_msg",
                "payload": {"type": "task_started", "turn_id": "turn-1", "started_at": 1_782_000_000i64, "model_context_window": 258400}
            }),
            serde_json::json!({
                "timestamp": "2026-01-03T00:00:02.000Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call", "name": "exec_command",
                    "arguments": "{\"cmd\":\"cargo nextest run quarkonium\",\"workdir\":\"/w\"}",
                    "call_id": "call-exec-1",
                    "internal_chat_message_metadata_passthrough": {"turn_id": "turn-1"}
                }
            }),
            serde_json::json!({
                "timestamp": "2026-01-03T00:00:02.100Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call_output", "call_id": "call-exec-1",
                    "output": "Wall time: 2.5000 seconds\nProcess exited with code 0\nOutput:\ntest result: ok\n"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-01-03T00:00:03.000Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call", "name": "update_plan", "call_id": "call-plan-1",
                    "arguments": "{\"plan\":[{\"step\":\"sweep telemetry\",\"status\":\"in_progress\"},{\"step\":\"ship\",\"status\":\"pending\"}]}"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-01-03T00:00:04.000Z",
                "type": "event_msg",
                "payload": {
                    "type": "patch_apply_end", "call_id": "call-patch-1", "turn_id": "turn-1", "success": true,
                    "stdout": "Success. Updated the following files:\nM src/quarkonium.rs\n",
                    "changes": {"src/quarkonium.rs": {"type": "update", "unified_diff": "@@ -1,2 +1,2 @@\n-a\n+b\n"}}
                }
            }),
            serde_json::json!({
                "timestamp": "2026-01-03T00:00:05.000Z",
                "type": "event_msg",
                "payload": {
                    "type": "mcp_tool_call_end", "call_id": "call-mcp-1", "plugin_id": "tracedecay@personal",
                    "invocation": {"server": "tracedecay", "tool": "tracedecay_context", "arguments": {"task": "quarkonium"}},
                    "duration": {"secs": 1, "nanos": 500000000},
                    "result": {"Ok": {"content": []}}
                }
            }),
            serde_json::json!({
                "timestamp": "2026-01-03T00:00:06.000Z",
                "type": "event_msg",
                "payload": {
                    "type": "web_search_end", "call_id": "call-ws-1", "query": "quarkonium decay width",
                    "action": {"type": "search", "queries": ["quarkonium decay width", "bottomonium spectrum"]}
                }
            }),
            serde_json::json!({
                "timestamp": "2026-01-03T00:00:07.000Z",
                "type": "event_msg",
                "payload": {"type": "sub_agent_activity", "event_id": "e1", "agent_thread_id": "thread-sub-1", "agent_path": "/root/telemetry_worker", "kind": "started"}
            }),
            serde_json::json!({
                "timestamp": "2026-01-03T00:00:08.000Z",
                "type": "inter_agent_communication",
                "payload": {"author": "/root/telemetry_worker", "recipient": "/root", "content": "", "encrypted_content": "gAAAAquarksecret", "trigger_turn": false}
            }),
            serde_json::json!({
                "timestamp": "2026-01-03T00:00:09.000Z",
                "type": "event_msg",
                "payload": {"type": "task_complete", "turn_id": "turn-1", "duration_ms": 8000, "time_to_first_token_ms": 900, "last_agent_message": "quarkonium sweep complete"}
            }),
            serde_json::json!({
                "timestamp": "2026-01-03T00:00:09.500Z",
                "type": "event_msg",
                "payload": {"type": "agent_message", "message": "The quarkonium telemetry sweep is complete."}
            }),
            serde_json::json!({
                "timestamp": "2026-01-03T00:00:10.000Z",
                "type": "event_msg",
                "payload": {"type": "token_count", "info": {
                    "model_context_window": 258400,
                    "last_token_usage": {"input_tokens": 100, "output_tokens": 20, "total_tokens": 120},
                    "rate_limits": {"primary": {"used_percent": 11.0, "resets_at": 1_780_375_431i64}, "secondary": {"used_percent": 30.0, "resets_at": 1_780_848_095i64}, "plan_type": "pro"}
                }}
            }),
        ],
    );
    path
}

/// Seeds a `moved-session` row under another working directory, naming
/// `stored_transcript` (or the rollout itself), then runs user-scope passes
/// over the session's rollout until one reports no failure. Returns the
/// runtime and the number of passes that took.
async fn ingest_rollout_over_stored_session(
    tmp: &TempDir,
    stored_transcript: Option<&str>,
) -> (HostAdmissionTestRuntimeV1, usize) {
    let home = tmp.path().join("home");
    let general = tmp.path().join("general-chat");
    let stored_cwd = tmp.path().join("earlier-cwd");
    std::fs::create_dir_all(&general).unwrap();
    std::fs::create_dir_all(&stored_cwd).unwrap();
    let rollout = write_codex_rollout(&home, &general, "moved-session");
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path().join("profile"))
        .await
        .unwrap();
    let stored = tracedecay_sessions::runtime::SessionRecord {
        provider: "codex".to_owned(),
        session_id: "moved-session".to_owned(),
        project_key: "user".to_owned(),
        project_path: durable_project_path_key(&stored_cwd.to_string_lossy()),
        title: None,
        started_at: Some(1),
        ended_at: None,
        transcript_path: Some(
            stored_transcript.map_or_else(|| rollout.to_string_lossy().into_owned(), str::to_owned),
        ),
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    };
    assert!(
        runtime
            .upsert_session_for_test(HostAdmissionScope::Profile, &stored)
            .await
            .unwrap()
    );

    let mut passes = 0;
    loop {
        passes += 1;
        let outcome =
            run_user_provider_pass(&runtime, &home, SessionProvider::Codex, Vec::new()).await;
        if outcome.failures.is_empty() {
            break;
        }
        assert!(passes < 4, "still retrying: {:?}", outcome.failures);
    }
    // Settled: another pass has nothing to retry.
    ingest_user_provider(&runtime, &home, SessionProvider::Codex, Vec::new()).await;
    (runtime, passes)
}

async fn billing_messages(runtime: &HostAdmissionTestRuntimeV1) -> usize {
    runtime
        .search_session_messages_for_test(
            HostAdmissionScope::Profile,
            "codex",
            None,
            "billing pipeline",
            10,
        )
        .await
        .unwrap()
        .len()
}

/// A session row written earlier under another working directory (a hook
/// payload, LCM, or an older build) still names the same transcript. The
/// rollout's messages must project and the stored directory stays. Before the
/// fix each message was parked as a `project_path` session collision and every
/// pass reported `ingest_pass_backpressured` again.
#[tokio::test]
async fn user_scope_projects_a_codex_rollout_whose_cwd_differs_from_the_stored_session() {
    let tmp = TempDir::new().unwrap();
    let (runtime, passes) = ingest_rollout_over_stored_session(&tmp, None).await;

    assert_eq!(passes, 1, "one pass must settle, not retry");
    assert_eq!(billing_messages(&runtime).await, 2, "both messages project");
    let session = runtime
        .session_for_test(HostAdmissionScope::Profile, "codex", "moved-session")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        session.project_path,
        durable_project_path_key(&tmp.path().join("earlier-cwd").to_string_lossy())
    );
}

/// A stored session that names a different transcript is a genuine duplicate:
/// its messages are still refused as a collision. The parked collision must
/// not keep every later pass reporting `ingest_pass_backpressured`.
#[tokio::test]
async fn user_scope_settles_after_a_genuine_codex_session_collision() {
    let tmp = TempDir::new().unwrap();
    let (runtime, _) =
        ingest_rollout_over_stored_session(&tmp, Some("/elsewhere/other-rollout.jsonl")).await;

    assert_eq!(
        billing_messages(&runtime).await,
        0,
        "a conflicting transcript identity stays a collision"
    );
}
