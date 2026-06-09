use std::io::Write;

use tempfile::TempDir;
use tokensave::sessions::cursor::{
    ingest_cursor_transcript_event, ingest_cursor_transcript_event_capped, open_project_session_db,
    project_session_db_path,
};
use tokensave::sessions::SessionSearchScope;

fn init_project(tmp: &TempDir) -> std::path::PathBuf {
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir(project.join(".tokensave")).unwrap();
    std::fs::write(project.join(".tokensave/tokensave.db"), "").unwrap();
    project
}

fn cursor_event(project: &std::path::Path, transcript: &std::path::Path) -> serde_json::Value {
    serde_json::json!({
        "session_id": "parent-session",
        "conversation_id": "conversation-1",
        "transcript_path": transcript,
        "workspace_roots": [project],
        "model": "gpt-5.5"
    })
}

fn write_cursor_parent_with_subagent(tmp: &TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
    let transcripts_dir = tmp.path().join("agent-transcripts");
    std::fs::create_dir_all(&transcripts_dir).unwrap();
    let parent = transcripts_dir.join("parent-session.jsonl");
    std::fs::write(
        &parent,
        r#"{"role":"user","message":{"content":[{"type":"text","text":"Parent asks for orchard transcript research."}]}}
"#,
    )
    .unwrap();

    let subagent_dir = transcripts_dir.join("parent-session").join("subagents");
    std::fs::create_dir_all(&subagent_dir).unwrap();
    let subagent = subagent_dir.join("worker-1.jsonl");
    std::fs::write(
        &subagent,
        r#"{"role":"assistant","message":{"content":[{"type":"text","text":"Worker found orchard transcript evidence."}]}}
"#,
    )
    .unwrap();
    (parent, subagent)
}

#[tokio::test]
async fn cursor_transcript_ingest_populates_searchable_messages() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir(project.join(".tokensave")).unwrap();
    std::fs::write(project.join(".tokensave/tokensave.db"), "").unwrap();

    let transcript = tmp.path().join("cursor-session.jsonl");
    std::fs::write(
        &transcript,
        r#"{"role":"user","message":{"content":[{"type":"text","text":"Please check billing ingestion from Cursor transcripts."}]}}
{"role":"assistant","message":{"content":[{"type":"text","text":"The billing ingestion plan is ready."},{"type":"tool_use","name":"tokensave_context","input":{"task":"billing ingestion"}}]}}
"#,
    )
    .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let event = serde_json::json!({
        "session_id": "cursor-session",
        "conversation_id": "conversation-1",
        "transcript_path": transcript,
        "cwd": project,
        "model": "gpt-5.5"
    });

    let stats = ingest_cursor_transcript_event(&event.to_string(), &db).await;
    assert_eq!(stats.sessions_upserted, 1);
    assert_eq!(stats.messages_upserted, 2);
    assert!(project_session_db_path(&project).exists());

    let results = db
        .search_session_messages(
            "cursor",
            Some(project.to_string_lossy().as_ref()),
            "billing ingestion",
            10,
        )
        .await;
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].session.session_id, "cursor-session");
    assert_eq!(
        results[0].session.transcript_path.as_deref(),
        transcript.to_str()
    );
    assert!(results
        .iter()
        .any(|hit| hit.message.tool_names.as_deref() == Some("tokensave_context")));
}

#[tokio::test]
async fn cursor_transcript_ingest_preserves_structured_content_in_raw_lcm() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);

    let transcript = tmp.path().join("cursor-session.jsonl");
    let content = serde_json::json!([
        {"type": "text", "text": "Inspect this image payload."},
        {"type": "image_url", "image_url": {"url": "data:image/png;base64,abcd"}}
    ]);
    let tool_calls = serde_json::json!([
        {
            "id": "call-1",
            "type": "function",
            "function": {
                "name": "read_file",
                "arguments": {"path": "src/lib.rs"}
            }
        }
    ]);
    std::fs::write(
        &transcript,
        format!(
            "{}\n",
            serde_json::json!({
                "role": "assistant",
                "message": {
                    "id": "cursor-structured",
                    "role": "assistant",
                    "content": content,
                    "tool_calls": tool_calls
                }
            })
        ),
    )
    .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let event = serde_json::json!({
        "session_id": "cursor-session",
        "transcript_path": transcript,
        "workspace_roots": [project]
    });

    let stats = ingest_cursor_transcript_event(&event.to_string(), &db).await;
    assert_eq!(stats.messages_upserted, 1);

    let raw = db
        .lcm_load_raw_message("cursor", "cursor-structured")
        .await
        .expect("raw structured message should exist");
    assert_eq!(raw.content, serde_json::to_string(&content).unwrap());

    let metadata: serde_json::Value =
        serde_json::from_str(raw.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["tool_calls"], tool_calls);
}

#[tokio::test]
async fn cursor_transcript_ingest_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);

    let transcript = tmp.path().join("cursor-session.jsonl");
    std::fs::write(
        &transcript,
        r#"{"role":"user","message":{"content":[{"type":"text","text":"Remember the Cursor transcript parser decision."}]}}
"#,
    )
    .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let event = serde_json::json!({
        "session_id": "cursor-session",
        "transcript_path": transcript,
        "workspace_roots": [project]
    });

    // Ingestion is now incremental: the first call ingests the message and
    // records a parse offset, so a second call over the *unchanged* file is a
    // no-op rather than re-upserting the same row.
    let first = ingest_cursor_transcript_event(&event.to_string(), &db).await;
    let second = ingest_cursor_transcript_event(&event.to_string(), &db).await;
    assert_eq!(first.messages_upserted, 1);
    assert_eq!(second.messages_upserted, 0);

    let results = db
        .search_session_messages("cursor", None, "parser decision", 10)
        .await;
    assert_eq!(results.len(), 1);
}

#[tokio::test]
async fn cursor_transcript_ingest_reads_only_appended_lines() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);

    let transcript = tmp.path().join("cursor-session.jsonl");
    std::fs::write(
        &transcript,
        r#"{"role":"user","message":{"content":[{"type":"text","text":"First message about incremental ingestion."}]}}
"#,
    )
    .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let event = serde_json::json!({
        "session_id": "cursor-session",
        "transcript_path": transcript,
        "workspace_roots": [project]
    });

    let first = ingest_cursor_transcript_event(&event.to_string(), &db).await;
    assert_eq!(first.messages_upserted, 1);

    // Append a new line; only the appended line should be parsed/upserted.
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    file.write_all(
        b"{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Second message about incremental ingestion.\"}]}}\n",
    )
    .unwrap();
    drop(file);

    let second = ingest_cursor_transcript_event(&event.to_string(), &db).await;
    assert_eq!(second.messages_upserted, 1);

    let results = db
        .search_session_messages("cursor", None, "incremental ingestion", 10)
        .await;
    assert_eq!(results.len(), 2);
}

#[tokio::test]
async fn cursor_transcript_ingest_cap_defers_large_backlog() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);

    let transcript = tmp.path().join("cursor-session.jsonl");
    let large_text = "x".repeat(2048);
    std::fs::write(
        &transcript,
        format!(
            "{{\"role\":\"user\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{large_text}\"}}]}}}}\n"
        ),
    )
    .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let event = serde_json::json!({
        "session_id": "cursor-session",
        "transcript_path": transcript,
        "workspace_roots": [project]
    });

    let capped = ingest_cursor_transcript_event_capped(&event.to_string(), &db, Some(128)).await;
    assert_eq!(capped.messages_upserted, 0);

    let uncapped = ingest_cursor_transcript_event(&event.to_string(), &db).await;
    assert_eq!(uncapped.messages_upserted, 1);
}

#[tokio::test]
async fn cursor_transcript_ingest_defers_partial_final_line() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);

    let transcript = tmp.path().join("cursor-session.jsonl");
    // A complete first line followed by a partial (un-terminated) second line,
    // as can happen mid-flush while Cursor is still writing the transcript.
    let complete = "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Complete line about partial handling.\"}]}}\n";
    let partial = "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Partial line about partial handling.\"}]}}";
    std::fs::write(&transcript, format!("{complete}{partial}")).unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let event = serde_json::json!({
        "session_id": "cursor-session",
        "transcript_path": transcript,
        "workspace_roots": [project]
    });

    // The partial final line is left unconsumed.
    let first = ingest_cursor_transcript_event(&event.to_string(), &db).await;
    assert_eq!(first.messages_upserted, 1);

    // Once the trailing newline arrives, the previously-partial line is ingested.
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    file.write_all(b"\n").unwrap();
    drop(file);

    let second = ingest_cursor_transcript_event(&event.to_string(), &db).await;
    assert_eq!(second.messages_upserted, 1);

    let results = db
        .search_session_messages("cursor", None, "partial handling", 10)
        .await;
    assert_eq!(results.len(), 2);
}

#[tokio::test]
async fn cursor_subagent_transcript_ingests_as_child_session() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let (parent, _subagent) = write_cursor_parent_with_subagent(&tmp);

    let db = open_project_session_db(&project).await.unwrap();
    let event = cursor_event(&project, &parent);

    let stats = ingest_cursor_transcript_event(&event.to_string(), &db).await;
    assert_eq!(stats.sessions_upserted, 2);
    assert_eq!(stats.messages_upserted, 2);

    let child = db
        .get_session("cursor", "worker-1")
        .await
        .expect("subagent session should be stored");
    assert_eq!(child.parent_session_id.as_deref(), Some("parent-session"));
    assert!(child.is_subagent);
    assert_eq!(child.agent_id.as_deref(), Some("worker-1"));

    let results = db
        .search_session_messages("cursor", None, "orchard evidence", 10)
        .await;
    assert!(results.iter().any(|hit| {
        hit.session.session_id == "worker-1"
            && hit.session.parent_session_id.as_deref() == Some("parent-session")
    }));
}

#[tokio::test]
async fn cursor_capped_ingest_discovers_subagents() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let (parent, _subagent) = write_cursor_parent_with_subagent(&tmp);

    let db = open_project_session_db(&project).await.unwrap();
    let event = cursor_event(&project, &parent);

    let stats = ingest_cursor_transcript_event_capped(&event.to_string(), &db, Some(4096)).await;
    assert_eq!(stats.sessions_upserted, 2);
    assert_eq!(stats.messages_upserted, 2);

    let child = db
        .get_session("cursor", "worker-1")
        .await
        .expect("subagent session should be stored");
    assert_eq!(child.parent_session_id.as_deref(), Some("parent-session"));
    assert!(child.is_subagent);
}

#[tokio::test]
async fn cursor_subagent_ingestion_is_incremental_per_file() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let (parent, subagent) = write_cursor_parent_with_subagent(&tmp);

    let db = open_project_session_db(&project).await.unwrap();
    let event = cursor_event(&project, &parent);
    let first = ingest_cursor_transcript_event(&event.to_string(), &db).await;
    assert_eq!(first.messages_upserted, 2);

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&subagent)
        .unwrap();
    file.write_all(
        b"{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Worker appended orchard followup.\"}]}}\n",
    )
    .unwrap();
    drop(file);

    let second = ingest_cursor_transcript_event(&event.to_string(), &db).await;
    assert_eq!(second.sessions_upserted, 1);
    assert_eq!(second.messages_upserted, 1);

    let child_hits = db
        .search_session_messages_filtered(
            "cursor",
            None,
            "orchard",
            10,
            SessionSearchScope::SubagentsOnly,
            Some("parent-session"),
        )
        .await;
    assert_eq!(child_hits.len(), 2);
    assert!(child_hits
        .iter()
        .all(|hit| hit.session.session_id == "worker-1"));
}

#[tokio::test]
async fn cursor_parent_and_subagent_offsets_do_not_collide() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let (parent, _subagent) = write_cursor_parent_with_subagent(&tmp);

    let db = open_project_session_db(&project).await.unwrap();
    let event = cursor_event(&project, &parent);
    let stats = ingest_cursor_transcript_event(&event.to_string(), &db).await;
    assert_eq!(stats.messages_upserted, 2);

    let parent_message = db
        .get_session_message("cursor", "parent-session:0")
        .await
        .expect("parent offset-derived id should exist");
    let child_message = db
        .get_session_message("cursor", "worker-1:0")
        .await
        .expect("subagent offset-derived id should exist");

    assert_eq!(parent_message.session_id, "parent-session");
    assert_eq!(child_message.session_id, "worker-1");
    assert_ne!(parent_message.message_id, child_message.message_id);
}

#[tokio::test]
async fn cursor_task_tool_dispatch_prompt_becomes_searchable() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let transcript = tmp.path().join("cursor-session.jsonl");
    std::fs::write(
        &transcript,
        r#"{"role":"assistant","message":{"content":[{"type":"tool_use","id":"toolu-task-1","name":"Task","input":{"description":"Research TranscriptSource ingestion","prompt":"Find how TranscriptSource handles JSONL offsets","subagent_type":"generalPurpose"}}]}}
"#,
    )
    .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let event = serde_json::json!({
        "session_id": "cursor-session",
        "transcript_path": transcript,
        "workspace_roots": [project]
    });

    let stats = ingest_cursor_transcript_event(&event.to_string(), &db).await;
    assert_eq!(stats.messages_upserted, 1);

    let results = db
        .search_session_messages("cursor", None, "TranscriptSource offsets", 10)
        .await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].message.kind.as_deref(), Some("tool_dispatch"));
    let metadata: serde_json::Value =
        serde_json::from_str(results[0].message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["source"], "cursor_transcript");
    assert_eq!(metadata["tool_use_id"], "toolu-task-1");
}
