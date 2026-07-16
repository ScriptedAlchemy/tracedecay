use std::io::Write;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tracedecay::global_db::GlobalDb;
use tracedecay::global_db::ParseOffset;
use tracedecay::sessions::codex::CodexSource;
use tracedecay::sessions::cursor::{open_project_session_db, resolved_project_session_db_path};
use tracedecay::sessions::lcm::{
    LcmContentSlice, LcmDescribeRequest, LcmDescribeTarget, LcmExpandRequest, LcmExpandTarget,
};
use tracedecay::sessions::source::{StoredCursor, TranscriptSource, ingest_source};
use tracedecay::sessions::{SessionProvider, ingest_global_sources_for_provider};
use tracedecay::store::GlobalDbObservationStore;
use tracedecay_store::ObservationProjectionStore;

use crate::common::{EnvVarGuard, GLOBAL_DB_ENV_LOCK};
use crate::restart_atomicity::{durable_table_count, mark_test_project};
use crate::support::{
    assert_metadata_path_eq, create_git_repo_with_linked_worktree, init_git_repo, setup,
};

fn write_jsonl(path: &std::path::Path, lines: &[serde_json::Value]) {
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
    let db = GlobalDb::open_at(&tmp.path().join("user-sessions.db"))
        .await
        .unwrap();
    let source = CodexSource::with_home(&home).for_user_scope(None, vec![registered]);

    let stats = ingest_source(&db, &source, tmp.path(), None).await;

    assert_eq!(stats.sessions_upserted, 1);
    assert!(db.get_session("codex", "user-session").await.is_some());
    assert!(db.get_session("codex", "project-session").await.is_none());
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
    let db = GlobalDb::open_at(&tmp.path().join("user-sessions.db"))
        .await
        .unwrap();
    let source = CodexSource::with_home(&home).for_user_scope(None, vec![registered]);

    let stats = ingest_source(&db, &source, tmp.path(), None).await;

    assert!(stats.messages_upserted > 0);
    assert!(
        db.search_session_messages("codex", None, "registered project secret", 10)
            .await
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

    let db_a = GlobalDb::open_at(&tmp.path().join("project-a.db"))
        .await
        .unwrap();
    let db_b = GlobalDb::open_at(&tmp.path().join("project-b.db"))
        .await
        .unwrap();
    let source = CodexSource::with_home(&home);
    ingest_source(&db_a, &source, &project_a, None).await;
    ingest_source(&db_b, &source, &project_b, None).await;

    assert!(
        !db_a
            .search_session_messages("codex", None, "billing pipeline", 10)
            .await
            .is_empty()
    );
    assert!(
        db_a.search_session_messages("codex", None, "project beta private marker", 10)
            .await
            .is_empty()
    );
    assert!(
        db_b.search_session_messages("codex", None, "billing pipeline", 10)
            .await
            .is_empty()
    );
    assert!(
        !db_b
            .search_session_messages("codex", None, "project beta private marker", 10)
            .await
            .is_empty()
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
    let db = GlobalDb::open_at(&tmp.path().join("user-sessions.db"))
        .await
        .unwrap();
    let source = CodexSource::with_home(&home).for_user_scope(None, vec![registered]);

    ingest_source(&db, &source, tmp.path(), None).await;

    assert!(
        db.search_session_messages("codex", None, "billing pipeline", 10)
            .await
            .is_empty()
    );
    assert!(
        !db.search_session_messages("codex", None, "general chat private marker", 10)
            .await
            .is_empty()
    );
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

fn write_codex_rollout_with_goal_context(
    home: &std::path::Path,
    project: &std::path::Path,
    session: &str,
) -> std::path::PathBuf {
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("rollout-2026-01-01T00-00-15-{session}.jsonl"));
    let contents = format!(
        "{}\n{}\n{}\n{}\n",
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:15.000Z",
            "type": "session_meta",
            "payload": {"id": session, "cwd": project.to_string_lossy(), "model": "gpt-5.5"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:15.100Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "Current goal for this thread\nobjective: ensure all provider session messages are ingested\nremaining token budget: 12000"
                }]
            }
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:15.200Z",
            "type": "response_item",
            "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "duplicate assistant response"}]}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:16.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "Continue implementation"}
        }),
    );
    std::fs::write(&path, contents).unwrap();
    path
}

fn write_codex_rollout_with_non_goal_response_item(
    home: &std::path::Path,
    project: &std::path::Path,
    session: &str,
) -> std::path::PathBuf {
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("rollout-2026-01-01T00-00-16-{session}.jsonl"));
    let contents = format!(
        "{}\n{}\n{}\n",
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:16.000Z",
            "type": "session_meta",
            "payload": {"id": session, "cwd": project.to_string_lossy(), "model": "gpt-5.5"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:16.100Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "what is the current goal and remaining token budget?"
                }]
            }
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:17.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "Continue implementation"}
        }),
    );
    std::fs::write(&path, contents).unwrap();
    path
}

fn write_codex_rollout_with_compaction(
    home: &std::path::Path,
    project: &std::path::Path,
    session: &str,
) -> std::path::PathBuf {
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("rollout-2026-01-01T00-00-20-{session}.jsonl"));
    let contents = format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n",
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:20.000Z",
            "type": "session_meta",
            "payload": {"id": session, "cwd": project.to_string_lossy(), "model": "gpt-5.5"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:21.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "Map the release automation state"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:22.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "Release automation is mapped."}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:23.000Z",
            "type": "compacted",
            "payload": {
                "message": "",
                "replacement_history": [
                    {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Map the release automation state"}]},
                    {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Release automation is mapped."}]},
                    {"type": "compaction", "encrypted_content": "encrypted-codex-summary"}
                ]
            }
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:23.010Z",
            "type": "event_msg",
            "payload": {"type": "context_compacted"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:24.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "Continue after compaction"}
        }),
    );
    std::fs::write(&path, contents).unwrap();
    path
}

fn write_codex_rollout_with_response_item_tools(
    home: &std::path::Path,
    project: &std::path::Path,
    session: &str,
) -> std::path::PathBuf {
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("rollout-2026-01-01T00-00-18-{session}.jsonl"));
    let long_output = format!("{}{}", "A".repeat(2400), "\nerror: exact failure line\n");
    write_jsonl(
        &path,
        &[
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:18.000Z",
                "type": "session_meta",
                "payload": {"id": session, "cwd": project.to_string_lossy(), "model": "gpt-5.5"}
            }),
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:18.100Z",
                "type": "event_msg",
                "payload": {"type": "user_message", "message": "Inspect response item telemetry"}
            }),
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:18.200Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "name": "exec_command",
                    "arguments": "{\"cmd\":\"rg -n MEMORY.md ~/.codex/memories\",\"workdir\":\"/home/zack/projects/tracedecay\"}",
                    "call_id": "call-tool-1",
                    "status": "completed"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:18.300Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call_output",
                    "call_id": "call-tool-1",
                    "output": long_output,
                    "status": "completed"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:18.400Z",
                "type": "response_item",
                "payload": {
                    "type": "custom_tool_call",
                    "name": "apply_patch",
                    "input": "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch\n",
                    "call_id": "call-tool-2",
                    "status": "completed"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:18.500Z",
                "type": "response_item",
                "payload": {
                    "type": "tool_search_call",
                    "call_id": "call-tool-3",
                    "arguments": {"query": "tracedecay context", "limit": 8},
                    "status": "completed"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:18.600Z",
                "type": "response_item",
                "payload": {
                    "type": "web_search_call",
                    "call_id": "call-tool-4",
                    "action": {
                        "type": "search",
                        "query": "zxqvunicorntoken rust async runtime",
                        "queries": ["zxqvunicorntoken rust async runtime"]
                    },
                    "status": "completed"
                }
            }),
        ],
    );
    path
}

#[tokio::test]
async fn codex_goal_response_item_is_cataloged_as_context() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_codex_rollout_with_goal_context(&home, &project, "codex-goal-context");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);

    let stats = ingest_source(&db, &source, &project, None).await;
    assert_eq!(stats.messages_upserted, 2);

    let results = db
        .search_session_messages(
            "codex",
            Some(project.to_string_lossy().as_ref()),
            "provider session messages",
            10,
        )
        .await;
    assert_eq!(results.len(), 1);
    let goal = &results[0].message;
    assert_eq!(goal.role, "system");
    assert_eq!(goal.kind.as_deref(), Some("context"));
    assert!(goal.text.contains("remaining token budget: 12000"));

    let metadata: serde_json::Value =
        serde_json::from_str(goal.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["source"], "codex_goal_context");
    assert_eq!(metadata["source_event"], "response_item");
}

#[tokio::test]
async fn codex_regular_response_item_goal_words_are_not_cataloged() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_codex_rollout_with_non_goal_response_item(&home, &project, "codex-non-goal-context");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);

    let stats = ingest_source(&db, &source, &project, None).await;
    assert_eq!(stats.messages_upserted, 1);

    let results = db
        .search_session_messages(
            "codex",
            Some(project.to_string_lossy().as_ref()),
            "remaining token budget",
            10,
        )
        .await;
    assert!(results.is_empty());
}

#[tokio::test]
async fn codex_response_item_tool_events_are_cataloged_compactly() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_codex_rollout_with_response_item_tools(&home, &project, "codex-response-item-tools");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);

    let stats = ingest_source(&db, &source, &project, None).await;
    // user_message + one joined exec_command tool_call (call+output collapse into
    // a single row) + custom_tool_call + tool_search_call + web_search_call.
    assert_eq!(stats.messages_upserted, 5);

    let results = db
        .search_session_messages(
            "codex",
            Some(project.to_string_lossy().as_ref()),
            "exec_command",
            10,
        )
        .await;
    assert_eq!(results.len(), 1);
    let call = &results[0].message;
    assert_eq!(call.role, "tool");
    // exec_command is now the structured `tool_call` kind, with the command as
    // the searchable text and the output body reduced to parsed fields.
    assert_eq!(call.kind.as_deref(), Some("tool_call"));
    assert_eq!(call.text, "rg -n MEMORY.md ~/.codex/memories");
    assert_eq!(call.tool_names.as_deref(), Some("exec_command"));

    let metadata: serde_json::Value =
        serde_json::from_str(call.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["source"], "codex_exec_command");
    assert_eq!(metadata["source_event"], "exec_command");
    assert_eq!(metadata["tool"], "exec_command");
    assert_eq!(metadata["call_id"], "call-tool-1");
    assert_eq!(metadata["cmd"], "rg -n MEMORY.md ~/.codex/memories");
    assert_eq!(metadata["workdir"], "/home/zack/projects/tracedecay");
    // The output carried no "Process exited with code" marker, so exit code and
    // success stay null rather than being guessed.
    assert_eq!(metadata["exit_code"], serde_json::Value::Null);
    assert_eq!(metadata["success"], serde_json::Value::Null);
    // The full output body (and its failure line) is never stored — only the
    // parsed fields — so heavy tool output does not bloat the index.
    assert!(!call.text.contains("error: exact failure line"));
    assert!(
        !call
            .metadata_json
            .as_deref()
            .unwrap()
            .contains("error: exact failure line")
    );

    // The row stays reversible: it points back at the exact call line.
    let rollout_name = "rollout-2026-01-01T00-00-18-codex-response-item-tools.jsonl";
    assert!(
        call.source_path
            .as_deref()
            .is_some_and(|path| path.ends_with(rollout_name))
    );
    let call_offset = call.source_offset.expect("tool_call carries source_offset");

    // web_search_call remains a generic tool_event (only event_msg
    // web_search_end is promoted to the `web_search` kind).
    let web_search_results = db
        .search_session_messages(
            "codex",
            Some(project.to_string_lossy().as_ref()),
            "web_search",
            10,
        )
        .await;
    assert_eq!(web_search_results.len(), 1);
    let web_search = &web_search_results[0].message;
    assert_eq!(web_search.kind.as_deref(), Some("tool_event"));
    assert_eq!(web_search.tool_names.as_deref(), Some("web_search"));
    assert!(!web_search.text.contains("zxqvunicorntoken"));
    assert!(web_search.text.contains("arguments_bytes:"));
    assert!(
        web_search
            .source_path
            .as_deref()
            .is_some_and(|path| path.ends_with(rollout_name))
    );
    // web_search_call is a later JSONL line than the exec_command call, so its
    // byte offset into the rollout is strictly greater.
    let web_search_offset = web_search
        .source_offset
        .expect("web_search row carries source_offset");
    assert!(web_search_offset > call_offset);
}

/// The new Codex CLI emits shell commands as a `custom_tool_call` named `exec`
/// whose `input` is a JS harness (`tools.exec_command({…})`) paired with a
/// `custom_tool_call_output`. `apply_patch` keeps the generic byte-counted path.
fn write_codex_rollout_with_custom_exec(
    home: &std::path::Path,
    project: &std::path::Path,
    session: &str,
) -> std::path::PathBuf {
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("rollout-2026-01-01T00-00-19-{session}.jsonl"));
    write_jsonl(
        &path,
        &[
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:19.000Z",
                "type": "session_meta",
                "payload": {"id": session, "cwd": project.to_string_lossy(), "model": "gpt-5.5"}
            }),
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:19.100Z",
                "type": "event_msg",
                "payload": {"type": "user_message", "message": "Finish the release work"}
            }),
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:19.200Z",
                "type": "response_item",
                "payload": {
                    "type": "custom_tool_call",
                    "id": "ctc_1",
                    "status": "completed",
                    "call_id": "call-exec-1",
                    "name": "exec",
                    "input": "const r = await tools.exec_command({\"cmd\":\"gh pr merge 366 --squash\",\"workdir\":\"/home/zack/projects/tracedecay\",\"yield_time_ms\":10000});\ntext(r.output);\n",
                    "internal_chat_message_metadata_passthrough": {"turn_id": "turn-exec-1"}
                }
            }),
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:19.300Z",
                "type": "response_item",
                "payload": {
                    "type": "custom_tool_call_output",
                    "call_id": "call-exec-1",
                    "output": [
                        {"type": "input_text", "text": "Script completed\nWall time 1.4 seconds\nOutput:\n"},
                        {"type": "input_text", "text": "zxqvsecrettoken merged pull request #366\n"}
                    ]
                }
            }),
            // apply_patch stays on the generic path (file edits come from
            // patch_apply_end, not this custom_tool_call).
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:19.400Z",
                "type": "response_item",
                "payload": {
                    "type": "custom_tool_call",
                    "name": "apply_patch",
                    "input": "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch\n",
                    "call_id": "call-patch-1",
                    "status": "completed"
                }
            }),
        ],
    );
    path
}

#[tokio::test]
async fn codex_custom_tool_call_exec_is_joined_into_searchable_tool_call() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_codex_rollout_with_custom_exec(&home, &project, "codex-custom-exec");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);

    let stats = ingest_source(&db, &source, &project, None).await;
    // user_message + one joined exec tool_call (call+output collapse into a
    // single row) + apply_patch generic tool_event.
    assert_eq!(stats.messages_upserted, 3);

    // The command text is searchable — the regression the fix targets.
    let results = db
        .search_session_messages(
            "codex",
            Some(project.to_string_lossy().as_ref()),
            "pr merge 366",
            10,
        )
        .await;
    assert_eq!(results.len(), 1, "the merge command is searchable");
    let call = &results[0].message;
    assert_eq!(call.role, "tool");
    assert_eq!(call.kind.as_deref(), Some("tool_call"));
    assert_eq!(call.text, "gh pr merge 366 --squash");
    assert_eq!(call.tool_names.as_deref(), Some("exec_command"));

    let metadata: serde_json::Value =
        serde_json::from_str(call.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["source"], "codex_exec_command");
    assert_eq!(metadata["tool"], "exec_command");
    assert_eq!(metadata["call_id"], "call-exec-1");
    assert_eq!(metadata["cmd"], "gh pr merge 366 --squash");
    assert_eq!(metadata["workdir"], "/home/zack/projects/tracedecay");
    assert_eq!(metadata["turn_id"], "turn-exec-1");
    assert_eq!(metadata["wall_time_s"], 1.4);
    // The custom harness header has no exit code, so it stays null.
    assert_eq!(metadata["exit_code"], serde_json::Value::Null);
    assert_eq!(metadata["success"], serde_json::Value::Null);
    // The output body (and anything secret in it) never lands in the index.
    assert!(!call.text.contains("zxqvsecrettoken"));
    assert!(
        !call
            .metadata_json
            .as_deref()
            .unwrap()
            .contains("zxqvsecrettoken")
    );

    // apply_patch stays a generic byte-counted tool_event, never an exec join.
    let patch_results = db
        .search_session_messages(
            "codex",
            Some(project.to_string_lossy().as_ref()),
            "call-patch-1",
            10,
        )
        .await;
    assert_eq!(patch_results.len(), 1);
    assert_eq!(patch_results[0].message.kind.as_deref(), Some("tool_event"));

    // Re-parsing the same rollout from the start is idempotent: the joined row
    // keys on the call offset, so it upserts rather than duplicating.
    let path_str = write_codex_rollout_with_custom_exec(&home, &project, "codex-custom-exec")
        .to_string_lossy()
        .to_string();
    db.set_parse_offset(
        &path_str,
        ParseOffset {
            byte_offset: 0,
            mtime: 1,
            file_id: 1,
        },
    )
    .await;
    ingest_source(&db, &source, &project, None).await;
    let after = db
        .search_session_messages(
            "codex",
            Some(project.to_string_lossy().as_ref()),
            "pr merge 366",
            10,
        )
        .await;
    assert_eq!(
        after.len(),
        1,
        "re-ingest does not duplicate the joined row"
    );
}

#[tokio::test]
async fn codex_response_item_skips_developer_messages_and_keeps_reasoning_summaries() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let session = "codex-response-item-reasoning";
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("rollout-2026-01-01T00-00-19-{session}.jsonl"));
    write_jsonl(
        &path,
        &[
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:19.000Z",
                "type": "session_meta",
                "payload": {"id": session, "cwd": project.to_string_lossy(), "model": "gpt-5.5"}
            }),
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:19.100Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "developer",
                    "content": [{"type": "input_text", "text": "SECRET_DEVELOPER_CONTEXT_SHOULD_NOT_INDEX"}]
                }
            }),
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:19.200Z",
                "type": "response_item",
                "payload": {
                    "type": "reasoning",
                    "summary": [],
                    "encrypted_content": "ENCRYPTED_REASONING_SHOULD_NOT_INDEX"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:19.300Z",
                "type": "response_item",
                "payload": {
                    "type": "reasoning",
                    "summary": [{"type": "summary_text", "text": "Reasoned that compact tool telemetry is useful."}],
                    "encrypted_content": "ENCRYPTED_REASONING_SHOULD_NOT_INDEX"
                }
            }),
        ],
    );

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);

    let stats = ingest_source(&db, &source, &project, None).await;
    assert_eq!(stats.messages_upserted, 1);

    let developer_results = db
        .search_session_messages(
            "codex",
            Some(project.to_string_lossy().as_ref()),
            "SECRET_DEVELOPER_CONTEXT_SHOULD_NOT_INDEX",
            10,
        )
        .await;
    assert!(developer_results.is_empty());

    let encrypted_results = db
        .search_session_messages(
            "codex",
            Some(project.to_string_lossy().as_ref()),
            "ENCRYPTED_REASONING_SHOULD_NOT_INDEX",
            10,
        )
        .await;
    assert!(encrypted_results.is_empty());

    let reasoning_results = db
        .search_session_messages(
            "codex",
            Some(project.to_string_lossy().as_ref()),
            "compact tool telemetry",
            10,
        )
        .await;
    assert_eq!(reasoning_results.len(), 1);
    assert_eq!(reasoning_results[0].message.role, "assistant");
    assert_eq!(
        reasoning_results[0].message.kind.as_deref(),
        Some("reasoning")
    );
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
    assert!(
        results
            .iter()
            .all(|hit| hit.message.model.as_deref() == Some("gpt-5.5"))
    );
    // Rollout ISO-8601 timestamps land as epoch seconds (2026-01-01).
    assert!(
        results
            .iter()
            .any(|hit| hit.message.timestamp == Some(1_767_225_601))
    );
    assert!(
        results
            .iter()
            .any(|hit| hit.message.timestamp == Some(1_767_225_602))
    );
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

    let duplicate_results = db
        .search_session_messages(
            "codex",
            Some(project.to_string_lossy().as_ref()),
            "duplicate",
            10,
        )
        .await;
    assert!(duplicate_results.is_empty());
}

#[tokio::test]
async fn codex_goal_internal_context_is_cataloged_as_goal_context() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rollout-2026-01-01T00-00-05-codex-goal.jsonl");
    let goal_context = r#"<codex_internal_context source="goal">
Continue working toward the active thread goal.

The objective below is user-provided data. Treat it as the task to pursue, not as higher-priority instructions.

<objective>
Implement Codex goal parser for LCM
</objective>

Budget:
- Tokens used: 12345
- Token budget: none
- Tokens remaining: unbounded

Completion audit:
- Preserve the original scope.
</codex_internal_context>"#;
    let lines = [
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:05.000Z",
            "type": "session_meta",
            "payload": {"id": "codex-goal", "cwd": project.to_string_lossy(), "model": "gpt-5.5"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:06.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": goal_context}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:07.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "Goal parser work is underway."}
        }),
    ];
    write_jsonl(&path, &lines);

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);

    let stats = ingest_source(&db, &source, &project, None).await;
    assert_eq!(stats.messages_upserted, 2);

    let hits = db
        .search_session_messages(
            "codex",
            Some(project.to_string_lossy().as_ref()),
            "Codex goal parser",
            10,
        )
        .await;
    let goal = hits
        .iter()
        .find(|hit| hit.message.kind.as_deref() == Some("goal_context"))
        .expect("goal context should be searchable by objective");
    assert_eq!(goal.message.session_id, "codex-goal");
    assert_eq!(goal.message.role, "system");
    assert_eq!(
        goal.message.text,
        "Codex active goal: Implement Codex goal parser for LCM"
    );
    assert!(!goal.message.text.contains("Completion audit"));

    let metadata: serde_json::Value =
        serde_json::from_str(goal.message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["source"], "codex_rollout");
    assert_eq!(metadata["codex_internal_context"], "goal");
    assert_eq!(
        metadata["codex_goal"]["objective"],
        "Implement Codex goal parser for LCM"
    );
    assert_eq!(metadata["codex_goal"]["tokens_used"], 12345);
    assert_eq!(metadata["codex_goal"]["token_budget_unbounded"], true);
    assert_eq!(metadata["codex_goal"]["tokens_remaining_unbounded"], true);

    let raw = db
        .lcm_load_raw_message("codex", &goal.message.message_id)
        .await
        .expect("goal context should be cataloged in raw LCM");
    assert_eq!(raw.role, "system");
    assert_eq!(
        raw.content,
        "Codex active goal: Implement Codex goal parser for LCM"
    );
    assert!(!raw.content.contains("Preserve the original scope"));
    let raw_metadata: serde_json::Value =
        serde_json::from_str(raw.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(raw_metadata["codex_internal_context"], "goal");

    let boilerplate_hits = db
        .search_session_messages(
            "codex",
            Some(project.to_string_lossy().as_ref()),
            "\"Preserve the original scope\"",
            10,
        )
        .await;
    assert!(boilerplate_hits.is_empty());
}

#[tokio::test]
async fn codex_response_item_goal_context_is_cataloged_without_duplicate_messages() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rollout-2026-01-01T00-00-08-codex-response-goal.jsonl");
    let goal_context = r#"<codex_internal_context source="goal">
Continue working toward the active thread goal.

<objective>
Index Codex response item goals
</objective>

Budget:
- Tokens used: 77
- Token budget: 60000
- Tokens remaining: 59923
</codex_internal_context>"#;
    let lines = [
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:08.000Z",
            "type": "session_meta",
            "payload": {"id": "codex-response-goal", "cwd": project.to_string_lossy(), "model": "gpt-5.5"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:09.000Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": goal_context}]
            }
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:10.000Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "ordinary response item duplicate should stay skipped"}]
            }
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:11.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "Visible assistant reply"}
        }),
    ];
    write_jsonl(&path, &lines);

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);

    let stats = ingest_source(&db, &source, &project, None).await;
    assert_eq!(stats.messages_upserted, 2);

    let hits = db
        .search_session_messages(
            "codex",
            Some(project.to_string_lossy().as_ref()),
            "response item goals",
            10,
        )
        .await;
    let goal = hits
        .iter()
        .find(|hit| hit.message.kind.as_deref() == Some("goal_context"))
        .expect("response_item goal context should be searchable");
    assert_eq!(
        goal.message.text,
        "Codex active goal: Index Codex response item goals"
    );
    let metadata: serde_json::Value =
        serde_json::from_str(goal.message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["source_event"], "response_item");
    assert_eq!(metadata["source_role"], "user");
    assert_eq!(metadata["codex_goal"]["token_budget"], 60000);
    assert_eq!(metadata["codex_goal"]["tokens_remaining"], 59923);

    let duplicate_hits = db
        .search_session_messages(
            "codex",
            Some(project.to_string_lossy().as_ref()),
            "\"ordinary duplicate should stay skipped\"",
            10,
        )
        .await;
    assert!(duplicate_hits.is_empty());
}

#[tokio::test]
async fn codex_context_compaction_creates_lcm_summary_node() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_codex_rollout_with_compaction(&home, &project, "codex-compact");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);

    let stats = ingest_source(&db, &source, &project, None).await;
    assert_eq!(stats.messages_upserted, 4);

    let status = db.lcm_status("codex", Some("codex-compact")).await.unwrap();
    assert_eq!(status.raw_message_count, 4);
    assert_eq!(status.summary_node_count, 1);
    assert!(status.dag.depths.values().any(|depth| depth.count == 1));

    let description = db
        .lcm_describe(LcmDescribeRequest {
            provider: "codex".to_string(),
            session_id: "codex-compact".to_string(),
            target: LcmDescribeTarget::Session,
        })
        .await
        .unwrap();
    assert_eq!(description.summary_nodes.len(), 1);
    assert_eq!(description.summary_nodes[0].depth, 1);
    assert_eq!(description.summary_nodes[0].source_count, 2);

    let node_id = description.summary_nodes[0].node_id.clone();
    let expanded = db
        .lcm_describe(LcmDescribeRequest {
            provider: "codex".to_string(),
            session_id: "codex-compact".to_string(),
            target: LcmDescribeTarget::SummaryNode { node_id },
        })
        .await
        .unwrap();
    let summary = expanded.summary_node.expect("summary node should expand");
    assert_eq!(summary.source_count, 2);

    let expansion = db
        .lcm_expand(LcmExpandRequest {
            provider: "codex".to_string(),
            session_id: "codex-compact".to_string(),
            target: LcmExpandTarget::SummaryNode {
                node_id: summary.node_id.clone(),
            },
            content_slice: Some(LcmContentSlice {
                offset: 0,
                limit: 1024,
            }),
            source_offset: 0,
            source_limit: Some(10),
        })
        .await
        .unwrap();
    assert!(
        expansion
            .content
            .contains("Map the release automation state")
    );
    assert!(expansion.content.contains("Release automation is mapped"));
    assert!(!expansion.content.contains("Summary body is encrypted"));
    assert_eq!(expansion.summary_sources.len(), 2);
}

#[tokio::test]
async fn repeated_codex_compactions_only_source_messages_since_previous_boundary() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rollout-2026-01-01T00-00-30-codex-repeat.jsonl");
    let cwd = project.to_string_lossy();
    let compact = |at: &str| {
        serde_json::json!({
            "timestamp": at,
            "type": "compacted",
            "payload": {
                "message": "",
                "replacement_history": [
                    {"type": "compaction", "encrypted_content": "encrypted"}
                ]
            }
        })
    };
    let lines = [
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:30.000Z",
            "type": "session_meta",
            "payload": {"id": "codex-repeat", "cwd": cwd, "model": "gpt-5.5"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:31.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "First compacted prompt"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:32.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "First compacted reply"}
        }),
        compact("2026-01-01T00:00:33.000Z"),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:34.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "Second compacted prompt"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:35.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "Second compacted reply"}
        }),
        compact("2026-01-01T00:00:36.000Z"),
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
    let stats = ingest_source(&db, &source, &project, None).await;
    assert_eq!(stats.messages_upserted, 6);

    let description = db
        .lcm_describe(LcmDescribeRequest {
            provider: "codex".to_string(),
            session_id: "codex-repeat".to_string(),
            target: LcmDescribeTarget::Session,
        })
        .await
        .unwrap();
    assert_eq!(description.summary_nodes.len(), 2);
    let source_counts = description
        .summary_nodes
        .iter()
        .map(|node| (node.depth, node.source_count))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(source_counts.get(&1), Some(&2));
    assert_eq!(source_counts.get(&2), Some(&2));
}

#[tokio::test]
async fn incremental_codex_compaction_depth_continues_from_prior_history() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rollout-2026-01-01T00-00-40-codex-incremental.jsonl");
    let cwd = project.to_string_lossy();
    let compact = |at: &str| {
        serde_json::json!({
            "timestamp": at,
            "type": "compacted",
            "payload": {
                "message": "",
                "replacement_history": [
                    {"type": "compaction", "encrypted_content": "encrypted"}
                ]
            }
        })
    };
    let first = [
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:40.000Z",
            "type": "session_meta",
            "payload": {"id": "codex-incremental", "cwd": cwd, "model": "gpt-5.5"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:41.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "First incremental prompt"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:42.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "First incremental reply"}
        }),
        compact("2026-01-01T00:00:43.000Z"),
    ];
    std::fs::write(
        &path,
        first
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    let stats = ingest_source(&db, &source, &project, None).await;
    assert_eq!(stats.messages_upserted, 3);

    let second = [
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:44.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "Second incremental prompt"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:45.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "Second incremental reply"}
        }),
        compact("2026-01-01T00:00:46.000Z"),
    ];
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    for line in second {
        writeln!(file, "{line}").unwrap();
    }

    let stats = ingest_source(&db, &source, &project, None).await;
    assert_eq!(stats.messages_upserted, 3);

    let description = db
        .lcm_describe(LcmDescribeRequest {
            provider: "codex".to_string(),
            session_id: "codex-incremental".to_string(),
            target: LcmDescribeTarget::Session,
        })
        .await
        .unwrap();
    let depths = description
        .summary_nodes
        .iter()
        .map(|node| node.depth)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(depths, [1, 2].into_iter().collect());
}

#[tokio::test]
async fn codex_compaction_depth_resets_when_rollout_replays_from_start() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rollout-2026-01-01T00-00-45-codex-replay.jsonl");
    let cwd = project.to_string_lossy();
    let compact = |at: &str| {
        serde_json::json!({
            "timestamp": at,
            "type": "compacted",
            "payload": {
                "message": "",
                "replacement_history": [
                    {"type": "compaction", "encrypted_content": "encrypted"}
                ]
            }
        })
    };
    let lines = [
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:45.000Z",
            "type": "session_meta",
            "payload": {"id": "codex-replay", "cwd": cwd, "model": "gpt-5.5"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:46.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "First replay prompt"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:47.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "First replay reply"}
        }),
        compact("2026-01-01T00:00:48.000Z"),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:49.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "Second replay prompt"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:50.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "Second replay reply"}
        }),
        compact("2026-01-01T00:00:51.000Z"),
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
    let path_str = path.to_string_lossy().to_string();
    db.set_parse_offset(
        &path_str,
        ParseOffset {
            byte_offset: std::fs::metadata(&path).unwrap().len(),
            mtime: 1,
            file_id: 1,
        },
    )
    .await;

    let stats = ingest_source(&db, &source, &project, None).await;
    assert_eq!(stats.messages_upserted, 6);

    let description = db
        .lcm_describe(LcmDescribeRequest {
            provider: "codex".to_string(),
            session_id: "codex-replay".to_string(),
            target: LcmDescribeTarget::Session,
        })
        .await
        .unwrap();
    let depths = description
        .summary_nodes
        .iter()
        .map(|node| node.depth)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(depths, [1, 2].into_iter().collect());
}

#[tokio::test]
async fn codex_compaction_summary_can_be_replaced_with_auxiliary_summary() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_codex_rollout_with_compaction(&home, &project, "codex-compact");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    ingest_source(&db, &source, &project, None).await;

    let pending = db
        .pending_codex_compaction_summary_requests(Some("codex-compact"), 10)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].request.provider, "codex");
    assert_eq!(pending[0].request.session_id, "codex-compact");
    assert_eq!(
        pending[0]
            .request
            .source_messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec![
            "Map the release automation state",
            "Release automation is mapped."
        ]
    );

    let replacement = db
        .replace_codex_compaction_summary(
            &pending[0].node_id,
            "Auxiliary Codex app-server summary",
            "codex_app_server",
            Some("gpt-5.4"),
        )
        .await
        .unwrap();
    assert_eq!(
        replacement.summary_text,
        "Auxiliary Codex app-server summary"
    );
    assert_ne!(replacement.node_id, pending[0].node_id);
    assert_eq!(replacement.source_refs.len(), 2);

    let pending_after = db
        .pending_codex_compaction_summary_requests(Some("codex-compact"), 10)
        .await
        .unwrap();
    assert!(pending_after.is_empty());

    let status = db.lcm_status("codex", Some("codex-compact")).await.unwrap();
    assert_eq!(status.summary_node_count, 1);
}

#[tokio::test]
async fn codex_compaction_summary_replacement_rolls_back_and_reuses_writer_after_failure() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_codex_rollout_with_compaction(&home, &project, "codex-compact-rollback");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    ingest_source(&db, &source, &project, None).await;
    let pending = db
        .pending_codex_compaction_summary_requests(Some("codex-compact-rollback"), 10)
        .await
        .unwrap();
    let original_node_id = pending[0].node_id.clone();

    let db_path = resolved_project_session_db_path(&project).await.unwrap();
    let trigger_db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let trigger_conn = trigger_db.connect().unwrap();
    trigger_conn
        .execute_batch(
            "CREATE TRIGGER fail_codex_summary_replacement
             BEFORE INSERT ON lcm_summary_nodes
             BEGIN
                SELECT RAISE(ABORT, 'forced summary replacement failure');
             END;",
        )
        .await
        .unwrap();

    let error = db
        .replace_codex_compaction_summary(
            &original_node_id,
            "Failed replacement",
            "codex_app_server",
            None,
        )
        .await
        .expect_err("trigger should abort replacement");
    assert!(
        format!("{error:?}").contains("forced summary replacement failure"),
        "unexpected error: {error:?}"
    );
    let pending_after_failure = db
        .pending_codex_compaction_summary_requests(Some("codex-compact-rollback"), 10)
        .await
        .unwrap();
    assert_eq!(pending_after_failure[0].node_id, original_node_id);

    trigger_conn
        .execute_batch("DROP TRIGGER fail_codex_summary_replacement;")
        .await
        .unwrap();
    drop(trigger_conn);
    drop(trigger_db);

    let replacement = db
        .replace_codex_compaction_summary(
            &original_node_id,
            "Successful replacement",
            "codex_app_server",
            None,
        )
        .await
        .expect("writer should remain reusable after rollback");
    assert_eq!(replacement.summary_text, "Successful replacement");
    assert_ne!(replacement.node_id, original_node_id);
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
    ingest_source(&db, &source, &project, None).await;

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
async fn codex_messages_keep_turn_cwd_and_session_git_updates() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let linked_worktree = tmp.path().join("linked-worktree");
    create_git_repo_with_linked_worktree(&project, &linked_worktree);
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rollout-2026-01-01T00-00-00-branch-sess.jsonl");
    let main_cwd = project.to_string_lossy();
    let linked_cwd = linked_worktree.to_string_lossy();
    write_jsonl(
        &path,
        &[
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:00.000Z",
                "type": "session_meta",
                "payload": {
                    "id": "branch-sess",
                    "cwd": main_cwd,
                    "model_provider": "openai",
                    "git": {
                        "branch": "main",
                        "commit_hash": "1111111111111111111111111111111111111111",
                        "repository_url": "git@example.com:repo/project.git"
                    }
                }
            }),
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:00.500Z",
                "type": "turn_context",
                "payload": {"turn_id": "t1", "cwd": main_cwd, "model": "gpt-5.3-codex"}
            }),
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "event_msg",
                "payload": {"type": "user_message", "message": "First branch attribution marker"}
            }),
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:02.000Z",
                "type": "session_meta",
                "payload": {
                    "id": "branch-sess",
                    "cwd": main_cwd,
                    "model_provider": "openai",
                    "git": {
                        "branch": "feature/worktree",
                        "commit_hash": "2222222222222222222222222222222222222222",
                        "repository_url": "git@example.com:repo/project.git"
                    }
                }
            }),
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:02.500Z",
                "type": "turn_context",
                "payload": {"turn_id": "t2", "cwd": linked_cwd, "model": "gpt-5.5"}
            }),
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:03.000Z",
                "type": "event_msg",
                "payload": {
                    "type": "agent_message",
                    "message": "Second branch attribution marker"
                }
            }),
        ],
    );

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    ingest_source(&db, &source, &project, None).await;

    let hits = db
        .search_session_messages("codex", None, "attribution", 10)
        .await;
    assert_eq!(hits.len(), 2);
    let session_metadata: serde_json::Value =
        serde_json::from_str(hits[0].session.metadata_json.as_deref().unwrap()).unwrap();
    assert_metadata_path_eq(&session_metadata["codex_session_cwd"], &project);
    assert_metadata_path_eq(&session_metadata["codex_session_worktree"], &project);
    assert_eq!(
        session_metadata["codex_session_location_provenance"].as_str(),
        Some("session_meta")
    );
    assert_eq!(session_metadata["codex_git_branch"], "main");
    let metadata_of = |needle: &str| -> serde_json::Value {
        let hit = hits
            .iter()
            .find(|hit| hit.message.text.contains(needle))
            .unwrap_or_else(|| panic!("message containing {needle:?} should exist"));
        serde_json::from_str(hit.message.metadata_json.as_deref().unwrap()).unwrap()
    };

    let first = metadata_of("First branch");
    assert_metadata_path_eq(&first["codex_turn_cwd"], &project);
    assert_metadata_path_eq(&first["codex_turn_worktree"], &project);
    assert_eq!(
        first["codex_turn_location_provenance"].as_str(),
        Some("codex_context")
    );
    assert_eq!(first["codex_git_branch"], "main");
    assert_eq!(
        first["codex_git_commit_hash"],
        "1111111111111111111111111111111111111111"
    );

    let second = metadata_of("Second branch");
    assert_metadata_path_eq(&second["codex_turn_cwd"], &linked_worktree);
    assert_metadata_path_eq(&second["codex_turn_worktree"], &linked_worktree);
    assert_eq!(
        second["codex_turn_location_provenance"].as_str(),
        Some("codex_context")
    );
    assert_eq!(second["codex_git_branch"], "feature/worktree");
    assert_eq!(
        second["codex_git_commit_hash"],
        "2222222222222222222222222222222222222222"
    );
}

#[tokio::test]
async fn codex_incremental_ingest_reconstructs_prior_turn_cwd_and_git() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let linked_worktree = tmp.path().join("linked-worktree");
    create_git_repo_with_linked_worktree(&project, &linked_worktree);
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rollout-2026-01-01T00-00-00-branch-incremental.jsonl");
    let main_cwd = project.to_string_lossy();
    let linked_cwd = linked_worktree.to_string_lossy();
    let prior_lines = [
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:00.000Z",
            "type": "session_meta",
            "payload": {
                "id": "branch-incremental",
                "cwd": main_cwd,
                "model_provider": "openai",
                "git": {
                    "branch": "main",
                    "commit_hash": "1111111111111111111111111111111111111111"
                }
            }
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "First incremental branch marker"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:02.000Z",
            "type": "session_meta",
            "payload": {
                "id": "branch-incremental",
                "cwd": main_cwd,
                "model_provider": "openai",
                "git": {
                    "branch": "feature/worktree",
                    "commit_hash": "2222222222222222222222222222222222222222"
                }
            }
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:02.500Z",
            "type": "turn_context",
            "payload": {"turn_id": "t2", "cwd": linked_cwd, "model": "gpt-5.5"}
        }),
    ];
    let prior = prior_lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let resumed_line = serde_json::json!({
        "timestamp": "2026-01-01T00:00:03.000Z",
        "type": "event_msg",
        "payload": {
            "type": "agent_message",
            "message": "Second incremental branch marker"
        }
    })
    .to_string()
        + "\n";
    std::fs::write(&path, format!("{prior}{resumed_line}")).unwrap();

    let source = CodexSource::with_home(&home);
    let parsed = source
        .parse_new(
            &path,
            StoredCursor {
                position: prior.len() as u64,
                mtime: 0,
                file_id: 0,
            },
            &project,
            None,
        )
        .expect("resumed parse should produce the appended message");
    assert_eq!(parsed.messages.len(), 1);
    let metadata: serde_json::Value =
        serde_json::from_str(parsed.messages[0].metadata_json.as_deref().unwrap()).unwrap();
    assert_metadata_path_eq(&metadata["codex_turn_cwd"], &linked_worktree);
    assert_eq!(
        metadata["codex_turn_location_provenance"].as_str(),
        Some("codex_context")
    );
    assert_eq!(metadata["codex_git_branch"], "feature/worktree");
    assert_metadata_path_eq(&metadata["codex_turn_worktree"], &linked_worktree);
    assert_eq!(
        metadata["codex_git_commit_hash"],
        "2222222222222222222222222222222222222222"
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
    assert_eq!(child.agent_id.as_deref(), Some("Euler"));

    let results = db
        .search_session_messages("codex", None, "layout evidence", 10)
        .await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].session.session_id, "codex-child");
}

/// Writes a Codex rollout carrying a `thread_goal_updated` lifecycle: an
/// initial `active` goal, an identical follow-up (only token/time drift — must
/// be deduped), then a `paused` transition (a distinct state — must keep its
/// own row).
fn write_codex_rollout_with_goal_events(
    home: &std::path::Path,
    project: &std::path::Path,
    session: &str,
) -> std::path::PathBuf {
    let dir = home.join(".codex/sessions/2026/01/02");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("rollout-2026-01-02T00-00-00-{session}.jsonl"));
    let mut goal_events: Vec<serde_json::Value> = serde_json::from_str(include_str!(
        "../fixtures/provider_normalization/codex/thread_goal_updates.input.json"
    ))
    .expect("checked-in Codex goal update sequence");
    for event in &mut goal_events {
        event["payload"]["threadId"] = serde_json::Value::String(session.to_owned());
        event["payload"]["goal"]["threadId"] = serde_json::Value::String(session.to_owned());
    }
    let mut records = vec![
        serde_json::json!({
            "timestamp": "2026-01-02T00:00:00.000Z",
            "type": "session_meta",
            "payload": {"id": session, "cwd": project.to_string_lossy(), "model": "gpt-5.5"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-02T00:00:01.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "start the overhaul"}
        }),
    ];
    records.append(&mut goal_events);
    records.push(serde_json::json!({
        "timestamp": "2026-01-02T00:00:05.000Z",
        "type": "event_msg",
        "payload": {"type": "agent_message", "message": "paused for review"}
    }));
    write_jsonl(&path, &records);
    path
}

#[tokio::test]
async fn codex_thread_goal_events_ingested_as_goal_rows_with_dedupe() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_codex_rollout_with_goal_events(&home, &project, "codex-goal-events");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    let stats = ingest_source(&db, &source, &project, None).await;
    // user_message + agent_message + three goal transitions. The drift-only
    // active repeat is deduped; objective and status transitions remain.
    assert_eq!(stats.messages_upserted, 5);

    // Both distinct goal states are searchable by their shared objective text.
    let hits = db
        .search_session_messages(
            "codex",
            Some(project.to_string_lossy().as_ref()),
            "phlogiston pipeline",
            10,
        )
        .await;
    let goal_hits: Vec<_> = hits
        .iter()
        .filter(|hit| hit.message.kind.as_deref() == Some("goal"))
        .collect();
    assert_eq!(
        goal_hits.len(),
        3,
        "objective/status transitions kept, drift deduped"
    );
    let mut statuses: Vec<String> = goal_hits
        .iter()
        .filter_map(|hit| {
            let meta: serde_json::Value =
                serde_json::from_str(hit.message.metadata_json.as_deref().unwrap()).ok()?;
            meta.get("status")
                .and_then(|s| s.as_str())
                .map(str::to_string)
        })
        .collect();
    statuses.sort();
    assert_eq!(
        statuses,
        vec![
            "active".to_string(),
            "active".to_string(),
            "paused".to_string()
        ]
    );
    for hit in &goal_hits {
        assert_eq!(hit.message.role, "system");
        assert!(matches!(
            hit.message.text.as_str(),
            "phlogiston pipeline overhaul and reconciliation"
                | "phlogiston pipeline rollout and verification"
        ));
        let meta: serde_json::Value =
            serde_json::from_str(hit.message.metadata_json.as_deref().unwrap()).unwrap();
        assert_eq!(meta["source"], "codex_thread_goal");
        assert_eq!(meta["source_event"], "thread_goal_updated");
        assert_eq!(meta["thread_id"], "codex-goal-events");
    }
}

#[tokio::test]
async fn recent_session_goals_surfaces_latest_status_per_session() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_codex_rollout_with_goal_events(&home, &project, "codex-goal-events");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    ingest_source(&db, &source, &project, None).await;

    let goals = db
        .recent_session_goals(Some(project.to_string_lossy().as_ref()), 10)
        .await;
    // One row per session: the latest lifecycle state (paused).
    assert_eq!(goals.len(), 1);
    let goal = &goals[0];
    assert_eq!(goal.session.session_id, "codex-goal-events");
    assert_eq!(goal.message.kind.as_deref(), Some("goal"));
    assert_eq!(
        goal.message.text,
        "phlogiston pipeline rollout and verification"
    );
    let meta: serde_json::Value =
        serde_json::from_str(goal.message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(meta["status"], "paused");
    assert_eq!(meta["updated_at"], 1_782_880_661i64);

    // Re-ingest must be idempotent (upsert keyed by message_id): still one goal.
    ingest_source(&db, &source, &project, None).await;
    let goals_again = db
        .recent_session_goals(Some(project.to_string_lossy().as_ref()), 10)
        .await;
    assert_eq!(goals_again.len(), 1);
}

/// A rollout carrying the full spread of structured Codex telemetry: a turn
/// boundary pair, a joined `exec_command` tool call, a plan update, a patch
/// application, an MCP tool call, a web search, sub-agent activity, and an
/// encrypted inter-agent routing edge — plus a `turn_context` and a
/// `token_count` with rate limits feeding the session summary.
fn write_codex_rollout_with_structured_events(
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
                "payload": {"type": "user_message", "message": "quarkonium telemetry sweep"}
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

#[tokio::test]
async fn codex_structured_events_produce_full_row_mix() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_codex_rollout_with_structured_events(&home, &project, "codex-structured");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    let stats = ingest_source(&db, &source, &project, None).await;
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
    let again = ingest_source(&db, &source, &project, None).await;
    assert_eq!(again.messages_upserted, 0);
}

fn write_codex_rollout_at(
    home: &Path,
    project: &Path,
    session: &str,
    relative_dir: &str,
    file_name: &str,
) -> PathBuf {
    let dir = home.join(".codex").join(relative_dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(file_name);
    let contents = format!(
        "{}\n{}\n{}\n",
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
                "message": "The billing pipeline regression is fixed."
            }
        }),
    );
    std::fs::write(&path, contents).unwrap();
    path
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn codex_jsonl_path_relocation_keeps_session_identity_on_production_observation_path() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let _home = EnvVarGuard::set("HOME", &home);
    init_git_repo(&project);
    mark_test_project(&project);
    let session = "codex-path-reloc-prod";
    let original = write_codex_rollout_at(
        &home,
        &project,
        session,
        "sessions/2026/01/01",
        &format!("rollout-2026-01-01T00-00-00-{session}.jsonl"),
    );

    let db = open_project_session_db(&project).await.unwrap();
    assert_eq!(
        ingest_global_sources_for_provider(&db, &project, Some(SessionProvider::Codex))
            .await
            .messages_upserted,
        2
    );
    assert_eq!(db.session_message_count().await.unwrap(), 2);
    let observations_before = durable_table_count(&project, "observations").await;
    assert!(observations_before >= 1);
    assert!(db.get_session("codex", session).await.is_some());
    drop(db);

    // Relocate the same real transcript bytes to another Codex discovery path.
    let original_bytes = std::fs::read(&original).unwrap();
    let relocated = home.join(format!(
        ".codex/sessions/2026/02/02/rollout-relocated-{session}.jsonl"
    ));
    std::fs::create_dir_all(relocated.parent().unwrap()).unwrap();
    std::fs::write(&relocated, &original_bytes).unwrap();
    assert_ne!(original, relocated);
    std::fs::remove_file(&original).unwrap();

    let relocated_db = open_project_session_db(&project).await.unwrap();
    let retry =
        ingest_global_sources_for_provider(&relocated_db, &project, Some(SessionProvider::Codex))
            .await;
    // Content-addressed observation identity + session_meta.payload.id keep the
    // logical session stable across filesystem path relocation; redelivery is a
    // durable no-op (no overwrite / no duplicate searchable rows).
    assert_eq!(retry.messages_upserted, 0);
    assert_eq!(relocated_db.session_message_count().await.unwrap(), 2);
    assert_eq!(
        durable_table_count(&project, "observations").await,
        observations_before
    );
    assert!(relocated_db.get_session("codex", session).await.is_some());
    assert_eq!(
        relocated_db
            .search_session_messages("codex", None, "fixed", 10)
            .await
            .len(),
        1
    );
}

async fn codex_observation_json_blobs(project: &Path) -> Vec<String> {
    let db_path = resolved_project_session_db_path(project).await.unwrap();
    let raw = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = raw.connect().unwrap();
    let mut rows = conn
        .query("SELECT observation_json FROM observations", ())
        .await
        .unwrap();
    let mut values = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        values.push(row.get::<String>(0).unwrap());
    }
    values
}

async fn codex_workflow_fact_rows(project: &Path) -> Vec<(String, Option<String>, Option<String>)> {
    let db_path = resolved_project_session_db_path(project).await.unwrap();
    let raw = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = raw.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT semantic_kind, status, state
             FROM observation_workflow_facts
             ORDER BY observation_sequence, fact_ordinal",
            (),
        )
        .await
        .unwrap();
    let mut values = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        values.push((
            row.get(0).unwrap(),
            row.get(1).unwrap(),
            row.get(2).unwrap(),
        ));
    }
    values
}

async fn codex_workflow_fact_count(project: &Path) -> u64 {
    let db_path = resolved_project_session_db_path(project).await.unwrap();
    let raw = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = raw.connect().unwrap();
    let mut rows = conn
        .query("SELECT COUNT(*) FROM observation_workflow_facts", ())
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get::<u64>(0).unwrap()
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn codex_workflow_lifecycle_goal_plan_task_persist_on_production_observation_path() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let _home = EnvVarGuard::set("HOME", &home);
    init_git_repo(&project);
    mark_test_project(&project);

    // Fixture-backed rollouts already checked in via write helpers.
    write_codex_rollout_with_goal_events(&home, &project, "codex-wf-goal");
    write_codex_rollout_with_structured_events(&home, &project, "codex-wf-structured");
    write_codex_rollout_with_goal_context(&home, &project, "codex-wf-goal-context");

    let db = open_project_session_db(&project).await.unwrap();
    let _ = ingest_global_sources_for_provider(&db, &project, Some(SessionProvider::Codex)).await;
    drop(db);

    let blobs = codex_observation_json_blobs(&project).await;
    assert!(
        blobs
            .iter()
            .any(|blob| blob.contains("\"kind\":\"workflow_lifecycle\"")
                && blob.contains("\"semantic_kind\":\"goal\"")
                && blob.contains("phlogiston pipeline overhaul")),
        "nested thread_goal_updated must persist WorkflowLifecycle Goal"
    );
    assert!(
        blobs
            .iter()
            .any(|blob| blob.contains("\"kind\":\"workflow_lifecycle\"")
                && blob.contains("\"semantic_kind\":\"plan\"")
                && blob.contains("sweep telemetry")
                && blob.contains("call-plan-1")),
        "update_plan arguments must persist on WorkflowLifecycle Plan"
    );
    assert!(
        blobs
            .iter()
            .any(|blob| blob.contains("\"kind\":\"workflow_lifecycle\"")
                && blob.contains("\"semantic_kind\":\"task\"")
                && blob.contains("task_complete")
                && !blob.contains("last_agent_message")),
        "exact task_complete must persist without last_agent_message"
    );
    assert!(
        blobs
            .iter()
            .any(|blob| blob.contains("\"kind\":\"tool_invocation\"")
                && blob.contains("\"name\":\"update_plan\"")
                && blob.contains("\"kind\":\"workflow_lifecycle\"")
                && blob.contains("\"semantic_kind\":\"plan\"")
                && blob.contains("sweep telemetry")),
        "update_plan must co-locate ToolInvocation + WorkflowLifecycle Plan"
    );
    assert!(
        blobs.iter().any(|blob| {
            blob.contains("ensure all provider session messages are ingested")
                && blob.contains("\"kind\":\"message\"")
                && !blob.contains("\"semantic_kind\":\"goal\"")
        }),
        "goal-context response_item must remain Message-only (no WorkflowLifecycle Goal)"
    );
    assert!(
        !blobs
            .iter()
            .any(|blob| blob.contains("task_completed") || blob.contains("task_failed")),
        "lookalike task_completed/task_failed must not appear as lifecycle facts"
    );

    let workflow_rows = codex_workflow_fact_rows(&project).await;
    assert!(
        workflow_rows
            .iter()
            .any(|(kind, status, _)| kind == "goal" && status.as_deref() == Some("paused")),
        "projected goal status must carry native paused transition; got {workflow_rows:?}"
    );
    assert!(
        workflow_rows.iter().any(|(kind, _, _)| kind == "plan"),
        "projected plan row missing; got {workflow_rows:?}"
    );
    assert!(
        workflow_rows
            .iter()
            .any(|(kind, _, state)| kind == "task" && state.as_deref() == Some("task_complete")),
        "projected task_complete row missing; got {workflow_rows:?}"
    );

    // Exact-duplicate redelivery is a durable no-op (content-addressed ids).
    let observations_before = durable_table_count(&project, "observations").await;
    let workflow_before = codex_workflow_fact_count(&project).await;
    let db = open_project_session_db(&project).await.unwrap();
    let _ = ingest_global_sources_for_provider(&db, &project, Some(SessionProvider::Codex)).await;
    drop(db);
    assert_eq!(
        durable_table_count(&project, "observations").await,
        observations_before
    );
    assert_eq!(codex_workflow_fact_count(&project).await, workflow_before);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn codex_goal_token_ticks_retain_raw_observations_and_dedupe_projected_goal_state() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let _home = EnvVarGuard::set("HOME", &home);
    init_git_repo(&project);
    mark_test_project(&project);

    // Checked-in production sequence: active → token/time tick → objective
    // transition → paused.
    write_codex_rollout_with_goal_events(&home, &project, "codex-goal-dedupe");

    let db = open_project_session_db(&project).await.unwrap();
    let _ = ingest_global_sources_for_provider(&db, &project, Some(SessionProvider::Codex)).await;

    let blobs = codex_observation_json_blobs(&project).await;
    let goal_observations = blobs
        .iter()
        .filter(|blob| {
            blob.contains("\"kind\":\"workflow_lifecycle\"")
                && blob.contains("\"semantic_kind\":\"goal\"")
        })
        .count();
    assert_eq!(
        goal_observations, 4,
        "all goal updates, including the token/time tick, must persist raw"
    );

    let goal_rows: Vec<_> = codex_workflow_fact_rows(&project)
        .await
        .into_iter()
        .filter(|(kind, _, _)| kind == "goal")
        .collect();
    assert_eq!(
        goal_rows.len(),
        3,
        "projected goal state must keep transitions only; got {goal_rows:?}"
    );
    assert_eq!(goal_rows[0].1.as_deref(), Some("active"));
    assert_eq!(goal_rows[1].1.as_deref(), Some("active"));
    assert_eq!(goal_rows[2].1.as_deref(), Some("paused"));

    let goals = db.recent_session_goals(None, 10).await;
    assert_eq!(goals.len(), 1);
    assert_eq!(
        goals[0].message.text,
        "phlogiston pipeline rollout and verification"
    );
    let meta: serde_json::Value =
        serde_json::from_str(goals[0].message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(meta["status"], "paused");

    let observations = durable_table_count(&project, "observations").await;
    GlobalDbObservationStore::new(&db)
        .rebuild_projection(observations)
        .await
        .unwrap();
    drop(db);

    let goal_rows_rebuilt: Vec<_> = codex_workflow_fact_rows(&project)
        .await
        .into_iter()
        .filter(|(kind, _, _)| kind == "goal")
        .collect();
    assert_eq!(goal_rows_rebuilt.len(), 3);
    assert_eq!(goal_rows_rebuilt[0].1.as_deref(), Some("active"));
    assert_eq!(goal_rows_rebuilt[1].1.as_deref(), Some("active"));
    assert_eq!(goal_rows_rebuilt[2].1.as_deref(), Some("paused"));

    // Restart reopen: latest goal remains paused with objective text.
    let reopened = open_project_session_db(&project).await.unwrap();
    let goals_again = reopened.recent_session_goals(None, 10).await;
    assert_eq!(goals_again.len(), 1);
    assert_eq!(
        goals_again[0].message.text,
        "phlogiston pipeline rollout and verification"
    );
    let meta_again: serde_json::Value =
        serde_json::from_str(goals_again[0].message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(meta_again["status"], "paused");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn codex_workflow_lifecycle_secret_content_is_sanitized_before_persistence() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let _home = EnvVarGuard::set("HOME", &home);
    init_git_repo(&project);
    mark_test_project(&project);

    const SECRET: &str = "AKIACODEXLIFECYCLE01";
    let dir = home.join(".codex/sessions/2026/01/04");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rollout-2026-01-04T00-00-00-codex-wf-secret.jsonl");
    // Nested goal shape from write_codex_rollout_with_goal_events, with an
    // exact credential pattern embedded in the evidenced objective field.
    write_jsonl(
        &path,
        &[
            serde_json::json!({
                "timestamp": "2026-01-04T00:00:00.000Z",
                "type": "session_meta",
                "payload": {"id": "codex-wf-secret", "cwd": project.to_string_lossy(), "model": "gpt-5.5"}
            }),
            serde_json::json!({
                "timestamp": "2026-01-04T00:00:01.000Z",
                "type": "event_msg",
                "payload": {
                    "type": "thread_goal_updated",
                    "threadId": "codex-wf-secret",
                    "goal": {
                        "threadId": "codex-wf-secret",
                        "objective": format!("rotate access key {SECRET}"),
                        "status": "active",
                        "tokensUsed": 1,
                        "timeUsedSeconds": 1,
                        "createdAt": 1_783_500_000i64,
                        "updatedAt": 1_783_500_001i64
                    }
                }
            }),
        ],
    );

    let db = open_project_session_db(&project).await.unwrap();
    let _ = ingest_global_sources_for_provider(&db, &project, Some(SessionProvider::Codex)).await;
    drop(db);

    let blobs = codex_observation_json_blobs(&project).await;
    let joined = blobs.join("\n");
    assert!(
        joined.contains("workflow_lifecycle"),
        "secret-bearing goal must still admit a WorkflowLifecycle fact"
    );
    assert!(
        !joined.contains(SECRET),
        "secret-bearing goal content must be sanitized before observation persistence"
    );
}
