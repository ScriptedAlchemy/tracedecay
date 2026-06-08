use std::io::Write;

use tempfile::TempDir;
use tokensave::sessions::cursor::{
    ingest_cursor_transcript_event, open_project_session_db, project_session_db_path,
};

fn init_project(tmp: &TempDir) -> std::path::PathBuf {
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir(project.join(".tokensave")).unwrap();
    std::fs::write(project.join(".tokensave/tokensave.db"), "").unwrap();
    project
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
