use std::io::Write;

use tempfile::TempDir;
use tracedecay::global_db::GlobalDb;
use tracedecay::sessions::cursor::{
    cursor_project_slug, ingest_cursor_transcript_event, ingest_cursor_transcript_event_capped,
    open_project_session_db, project_session_db_path, CursorSweepSource,
};
use tracedecay::sessions::source::ingest_source;
use tracedecay::sessions::SessionSearchScope;

fn init_project(tmp: &TempDir) -> std::path::PathBuf {
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir(project.join(".tracedecay")).unwrap();
    std::fs::write(project.join(".tracedecay/tracedecay.db"), "").unwrap();
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
    std::fs::create_dir(project.join(".tracedecay")).unwrap();
    std::fs::write(project.join(".tracedecay/tracedecay.db"), "").unwrap();

    let transcript = tmp.path().join("cursor-session.jsonl");
    std::fs::write(
        &transcript,
        r#"{"role":"user","message":{"content":[{"type":"text","text":"Please check billing ingestion from Cursor transcripts."}]}}
{"role":"assistant","message":{"content":[{"type":"text","text":"The billing ingestion plan is ready."},{"type":"tool_use","name":"tracedecay_context","input":{"task":"billing ingestion"}}]}}
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
        .any(|hit| hit.message.tool_names.as_deref() == Some("tracedecay_context")));
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
async fn cursor_transcript_ingest_retries_after_mid_batch_db_failure() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let transcript = tmp.path().join("cursor-session.jsonl");
    std::fs::write(
        &transcript,
        r#"{"role":"user","message":{"content":[{"type":"text","text":"Replay this line after failure."}]}}
"#,
    )
    .unwrap();
    let event = serde_json::json!({
        "session_id": "cursor-session",
        "transcript_path": transcript,
        "workspace_roots": [project]
    });
    let db_path = project_session_db_path(&project);

    // Ensure schema exists, then deliberately break the raw-message table.
    drop(open_project_session_db(&project).await.unwrap());
    let broken = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let broken_conn = broken.connect().unwrap();
    broken_conn
        .execute("DROP TABLE session_messages", ())
        .await
        .unwrap();

    // Skip schema ensure so ingest runs against the broken table.
    let broken_db = GlobalDb::open_at_assuming_schema(&db_path).await.unwrap();
    let first = ingest_cursor_transcript_event(&event.to_string(), &broken_db).await;
    assert_eq!(first.sessions_upserted, 0);
    assert_eq!(first.messages_upserted, 0);

    // Re-opening with schema ensure repairs the dropped table; retry should
    // ingest the same line because the failed pass did not advance the cursor.
    let repaired_db = open_project_session_db(&project).await.unwrap();
    let second = ingest_cursor_transcript_event(&event.to_string(), &repaired_db).await;
    assert_eq!(second.sessions_upserted, 1);
    assert_eq!(second.messages_upserted, 1);

    let hits = repaired_db
        .search_session_messages("cursor", None, "Replay this line", 10)
        .await;
    assert_eq!(hits.len(), 1);
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
async fn cursor_transcript_ingest_uses_cwd_root_in_multi_root_workspace() {
    let tmp = TempDir::new().unwrap();
    let root_a = tmp.path().join("root-a");
    let root_b = tmp.path().join("root-b");
    std::fs::create_dir_all(root_a.join(".tracedecay")).unwrap();
    std::fs::create_dir_all(root_b.join(".tracedecay")).unwrap();
    std::fs::write(root_a.join(".tracedecay/tracedecay.db"), "").unwrap();
    std::fs::write(root_b.join(".tracedecay/tracedecay.db"), "").unwrap();
    let cwd_b = root_b.join("workspace");
    std::fs::create_dir_all(&cwd_b).unwrap();
    let transcript = root_b.join("cursor-session.jsonl");
    std::fs::write(
        &transcript,
        r#"{"role":"user","message":{"content":[{"type":"text","text":"Route this to root B."}]}}
"#,
    )
    .unwrap();

    let db = open_project_session_db(&root_b).await.unwrap();
    let event = serde_json::json!({
        "session_id": "cursor-session",
        "transcript_path": transcript,
        "workspace_roots": [root_a, root_b],
        "cwd": cwd_b
    });

    let stats = ingest_cursor_transcript_event(&event.to_string(), &db).await;
    assert_eq!(stats.messages_upserted, 1);
    let session = db
        .get_session("cursor", "cursor-session")
        .await
        .expect("session should be stored under root B");
    assert_eq!(session.project_path, root_b.to_string_lossy());
    assert_eq!(session.project_key, root_b.to_string_lossy());
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
async fn cursor_transcript_ingest_derives_timestamps_from_timestamp_tags() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);

    // Cursor transcripts carry no structured timestamps; the injected
    // <timestamp> tag in user prompts is the only per-message signal. The
    // assistant line between the two tags must inherit (carry forward) the
    // first tag's timestamp.
    let transcript = tmp.path().join("cursor-session.jsonl");
    std::fs::write(
        &transcript,
        r#"{"role":"user","message":{"content":[{"type":"text","text":"<timestamp>Wednesday, Jun 10, 2026, 9:11 AM (UTC+2)</timestamp>\nFirst day question about chronology."}]}}
{"role":"assistant","message":{"content":[{"type":"text","text":"First day answer about chronology."}]}}
{"role":"user","message":{"content":[{"type":"text","text":"<timestamp>Thursday, Jun 11, 2026, 8:00 AM (UTC+2)</timestamp>\nSecond day question about chronology."}]}}
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
    assert_eq!(stats.messages_upserted, 3);

    let mut hits = db
        .search_session_messages("cursor", None, "chronology", 10)
        .await;
    hits.sort_by_key(|hit| hit.message.ordinal);
    assert_eq!(hits.len(), 3);

    // 2026-06-10T09:11:00+02:00 and 2026-06-11T08:00:00+02:00.
    let day_one = 1_781_075_460;
    let day_two = 1_781_157_600;
    assert_eq!(hits[0].message.timestamp, Some(day_one));
    assert_eq!(hits[1].message.timestamp, Some(day_one));
    assert_eq!(hits[2].message.timestamp, Some(day_two));

    // The session window derives from the first/last message timestamps.
    let session = db.get_session("cursor", "cursor-session").await.unwrap();
    assert_eq!(session.started_at, Some(day_one));
    assert_eq!(session.ended_at, Some(day_two));

    // The LCM raw store (what the dashboard timeline reads) is dated too.
    let raw = db
        .lcm_load_raw_message("cursor", &hits[0].message.message_id)
        .await
        .expect("raw message should exist");
    assert_eq!(raw.timestamp, Some(day_one));

    // Cursor transcripts carry no token counters (verified against real
    // files); ingest must not fabricate a usage object for the savings tab.
    for hit in &hits {
        let metadata: serde_json::Value =
            serde_json::from_str(hit.message.metadata_json.as_deref().unwrap()).unwrap();
        assert!(
            metadata.get("usage").is_none(),
            "cursor rows are usage-free"
        );
    }
}

#[tokio::test]
async fn cursor_transcript_ingest_falls_back_to_file_mtime_without_tags() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);

    let transcript = tmp.path().join("cursor-session.jsonl");
    std::fs::write(
        &transcript,
        r#"{"role":"assistant","message":{"content":[{"type":"text","text":"Untagged line about mtime fallback."}]}}
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

    let hits = db
        .search_session_messages("cursor", None, "mtime fallback", 10)
        .await;
    assert_eq!(hits.len(), 1);
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();
    let timestamp = hits[0]
        .message
        .timestamp
        .expect("untagged lines must fall back to the transcript mtime");
    assert!(
        (now - timestamp).abs() < 300,
        "mtime fallback should be near now, got {timestamp} vs {now}"
    );
}

/// Writes a parent + subagent transcript pair in the real on-disk layout the
/// catch-up sweep scans: `<home>/.cursor/projects/<slug>/agent-transcripts/
/// <session>/<session>.jsonl` (+ `<session>/subagents/<child>.jsonl`).
fn write_sweep_fixture(
    home: &std::path::Path,
    project: &std::path::Path,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let slug = cursor_project_slug(project).unwrap();
    let session_dir = home
        .join(".cursor")
        .join("projects")
        .join(slug)
        .join("agent-transcripts")
        .join("sweep-session");
    let subagent_dir = session_dir.join("subagents");
    std::fs::create_dir_all(&subagent_dir).unwrap();
    let parent = session_dir.join("sweep-session.jsonl");
    std::fs::write(
        &parent,
        r#"{"role":"user","message":{"content":[{"type":"text","text":"Historic parent message about orchard catchup."}]}}
"#,
    )
    .unwrap();
    let child = subagent_dir.join("sweep-worker.jsonl");
    std::fs::write(
        &child,
        r#"{"role":"assistant","message":{"content":[{"type":"text","text":"Historic worker message about orchard catchup."}]}}
"#,
    )
    .unwrap();
    (parent, child)
}

#[tokio::test]
async fn cursor_sweep_ingests_historical_transcripts() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let home = tmp.path().join("home");
    write_sweep_fixture(&home, &project);

    let db = open_project_session_db(&project).await.unwrap();
    let sweep = CursorSweepSource::with_home(&home);
    let stats = ingest_source(&db, &sweep, &project, None).await;
    assert_eq!(stats.sessions_upserted, 2);
    assert_eq!(stats.messages_upserted, 2);

    let parent_session = db
        .get_session("cursor", "sweep-session")
        .await
        .expect("swept parent session should be stored");
    assert_eq!(parent_session.project_path, project.to_string_lossy());
    assert!(!parent_session.is_subagent);

    let child_session = db
        .get_session("cursor", "sweep-worker")
        .await
        .expect("swept subagent session should be stored");
    assert_eq!(
        child_session.parent_session_id.as_deref(),
        Some("sweep-session")
    );
    assert!(child_session.is_subagent);

    let hits = db
        .search_session_messages("cursor", None, "orchard catchup", 10)
        .await;
    assert_eq!(hits.len(), 2);
}

#[tokio::test]
async fn cursor_sweep_after_hook_ingest_is_noop() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let home = tmp.path().join("home");
    let (parent, _child) = write_sweep_fixture(&home, &project);

    let db = open_project_session_db(&project).await.unwrap();
    let event = serde_json::json!({
        "session_id": "sweep-session",
        "transcript_path": parent,
        "workspace_roots": [project],
        "cwd": project,
    });
    let hook = ingest_cursor_transcript_event(&event.to_string(), &db).await;
    assert_eq!(hook.sessions_upserted, 2);
    assert_eq!(hook.messages_upserted, 2);

    // The sweep shares the hook path's per-file parse offsets, so everything
    // the hook already ingested is a no-op: zero new sessions, zero new rows.
    let sweep = CursorSweepSource::with_home(&home);
    let stats = ingest_source(&db, &sweep, &project, None).await;
    assert_eq!(stats.sessions_upserted, 0);
    assert_eq!(stats.messages_upserted, 0);

    let hits = db
        .search_session_messages("cursor", None, "orchard catchup", 10)
        .await;
    assert_eq!(hits.len(), 2);
}

#[tokio::test]
async fn cursor_hook_after_sweep_is_noop() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let home = tmp.path().join("home");
    let (parent, _child) = write_sweep_fixture(&home, &project);

    let db = open_project_session_db(&project).await.unwrap();
    let sweep = CursorSweepSource::with_home(&home);
    let swept = ingest_source(&db, &sweep, &project, None).await;
    assert_eq!(swept.messages_upserted, 2);

    // A live hook firing on a transcript the sweep already ingested resumes
    // from the shared offset and re-ingests nothing.
    let event = serde_json::json!({
        "session_id": "sweep-session",
        "transcript_path": parent,
        "workspace_roots": [project],
        "cwd": project,
    });
    let hook = ingest_cursor_transcript_event(&event.to_string(), &db).await;
    assert_eq!(hook.sessions_upserted, 0);
    assert_eq!(hook.messages_upserted, 0);

    let hits = db
        .search_session_messages("cursor", None, "orchard catchup", 10)
        .await;
    assert_eq!(hits.len(), 2);
}

#[tokio::test]
async fn cursor_sweep_picks_up_lines_appended_after_hook_ingest() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let home = tmp.path().join("home");
    let (parent, _child) = write_sweep_fixture(&home, &project);

    let db = open_project_session_db(&project).await.unwrap();
    let event = serde_json::json!({
        "session_id": "sweep-session",
        "transcript_path": parent,
        "workspace_roots": [project],
        "cwd": project,
    });
    let hook = ingest_cursor_transcript_event(&event.to_string(), &db).await;
    assert_eq!(hook.messages_upserted, 2);

    // Lines appended after the last hook firing (e.g. while hooks were
    // uninstalled) are exactly what the catch-up sweep must reconcile.
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&parent)
        .unwrap();
    file.write_all(
        b"{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Appended orchard catchup line.\"}]}}\n",
    )
    .unwrap();
    drop(file);

    let sweep = CursorSweepSource::with_home(&home);
    let stats = ingest_source(&db, &sweep, &project, None).await;
    assert_eq!(stats.sessions_upserted, 1);
    assert_eq!(stats.messages_upserted, 1);

    let hits = db
        .search_session_messages("cursor", None, "orchard catchup", 10)
        .await;
    assert_eq!(hits.len(), 3);
}

#[tokio::test]
async fn cursor_sweep_prefers_subagent_copy_over_toplevel_duplicate() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let home = tmp.path().join("home");
    write_sweep_fixture(&home, &project);

    // Cursor also materializes the subagent session as a top-level
    // `<id>/<id>.jsonl` copy whose content drifts from the subagents/ copy
    // (different byte offsets => different derived message ids). The sweep
    // must ingest the session exactly once, from the subagent copy.
    let slug = cursor_project_slug(&project).unwrap();
    let duplicate_dir = home
        .join(".cursor")
        .join("projects")
        .join(slug)
        .join("agent-transcripts")
        .join("sweep-worker");
    std::fs::create_dir_all(&duplicate_dir).unwrap();
    std::fs::write(
        duplicate_dir.join("sweep-worker.jsonl"),
        r#"{"role":"user","message":{"content":[{"type":"text","text":"Drifted preamble line."}]}}
{"role":"assistant","message":{"content":[{"type":"text","text":"Historic worker message about orchard catchup."}]}}
"#,
    )
    .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let sweep = CursorSweepSource::with_home(&home);
    let stats = ingest_source(&db, &sweep, &project, None).await;
    assert_eq!(stats.sessions_upserted, 2);
    assert_eq!(stats.messages_upserted, 2);

    // The session keeps its subagent identity instead of being flipped into
    // a parentless top-level session by the duplicate copy.
    let child = db
        .get_session("cursor", "sweep-worker")
        .await
        .expect("subagent session should be stored");
    assert!(child.is_subagent);
    assert_eq!(child.parent_session_id.as_deref(), Some("sweep-session"));

    // Exactly one copy of the worker's message, and nothing from the
    // drifted duplicate.
    let hits = db
        .search_session_messages("cursor", None, "orchard catchup", 10)
        .await;
    let worker_hits = hits
        .iter()
        .filter(|hit| hit.session.session_id == "sweep-worker")
        .count();
    assert_eq!(worker_hits, 1);
    let drifted = db
        .search_session_messages("cursor", None, "Drifted preamble", 10)
        .await;
    assert!(drifted.is_empty());
}

#[tokio::test]
async fn cursor_sweep_skips_ambiguous_project_slug() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("work").join("foo-bar");
    std::fs::create_dir_all(project.join(".tracedecay")).unwrap();
    std::fs::write(project.join(".tracedecay/tracedecay.db"), "").unwrap();
    // A second *existing* directory that encodes to the same slug as the
    // project ("…-work-foo-bar"): the sweep must skip rather than guess
    // which workspace the slug's transcripts belong to.
    std::fs::create_dir_all(tmp.path().join("work").join("foo").join("bar")).unwrap();
    write_sweep_fixture(&home, &project);

    let db = open_project_session_db(&project).await.unwrap();
    let sweep = CursorSweepSource::with_home(&home);
    let stats = ingest_source(&db, &sweep, &project, None).await;
    assert_eq!(stats.sessions_upserted, 0);
    assert_eq!(stats.messages_upserted, 0);
    assert!(db.get_session("cursor", "sweep-session").await.is_none());
}

#[tokio::test]
async fn cursor_sweep_skips_projects_without_tracedecay() {
    let tmp = TempDir::new().unwrap();
    let scratch = init_project(&tmp);
    let home = tmp.path().join("home");
    let unindexed = tmp.path().join("unindexed");
    std::fs::create_dir_all(&unindexed).unwrap();
    write_sweep_fixture(&home, &unindexed);

    let db = open_project_session_db(&scratch).await.unwrap();
    let sweep = CursorSweepSource::with_home(&home);
    let skipped = ingest_source(&db, &sweep, &unindexed, None).await;
    assert_eq!(skipped.sessions_upserted, 0);
    assert_eq!(skipped.messages_upserted, 0);

    // Once the project is indexed, the same sweep picks its transcripts up.
    std::fs::create_dir_all(unindexed.join(".tracedecay")).unwrap();
    std::fs::write(unindexed.join(".tracedecay/tracedecay.db"), "").unwrap();
    let indexed = ingest_source(&db, &sweep, &unindexed, None).await;
    assert_eq!(indexed.sessions_upserted, 2);
    assert_eq!(indexed.messages_upserted, 2);
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
