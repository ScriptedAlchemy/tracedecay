use std::io::Write;

use tempfile::TempDir;
use tracedecay::sessions::claude::ClaudeSource;
use tracedecay::sessions::cline_like::ClineLikeSource;
use tracedecay::sessions::cursor::{
    ingest_cursor_transcript_event, open_project_session_db, project_session_db_path,
};
use tracedecay::sessions::source::ingest_source;

use crate::claude::write_claude_transcript;
use crate::cline_like::{parse_offset_for_task_history, vscode_storage_root, write_task};
use crate::support::{init_project, setup};

async fn set_session_message_projection_failure(project: &std::path::Path, enabled: bool) {
    let db = libsql::Builder::new_local(project_session_db_path(project))
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    let statement = if enabled {
        "CREATE TRIGGER fail_claude_suffix_projection
         BEFORE INSERT ON session_messages
         BEGIN
            SELECT RAISE(ABORT, 'projection failure');
         END;"
    } else {
        "DROP TRIGGER fail_claude_suffix_projection;"
    };
    conn.execute_batch(statement).await.unwrap();
}

#[tokio::test]
async fn claude_restart_ingests_only_the_appended_suffix() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let path = write_claude_transcript(&home, &project, "claude-restart");
    let path_key = path.to_string_lossy().to_string();

    let db = open_project_session_db(&project).await.unwrap();
    let first = ingest_source(&db, &ClaudeSource::with_home(&home), &project, None).await;
    assert_eq!(first.messages_upserted, 2);
    let first_offset = db.get_parse_offset(&path_key).await.unwrap();
    let first_session = db.get_session("claude", "claude-restart").await.unwrap();

    let mut transcript = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(
        transcript,
        "{}",
        serde_json::json!({
            "type": "user",
            "cwd": project,
            "sessionId": "claude-restart",
            "uuid": "u3",
            "timestamp": "2026-01-01T00:00:10.000Z",
            "message": {"role": "user", "content": "Verify the billing suffix."}
        })
    )
    .unwrap();
    drop(transcript);
    drop(db);

    set_session_message_projection_failure(&project, true).await;

    let rejected = open_project_session_db(&project).await.unwrap();
    let failed = ingest_source(&rejected, &ClaudeSource::with_home(&home), &project, None).await;
    assert_eq!(failed.messages_upserted, 0);
    assert_eq!(
        rejected.get_parse_offset(&path_key).await,
        Some(first_offset)
    );
    assert_eq!(rejected.session_message_count().await.unwrap(), 2);
    assert_eq!(
        rejected.get_session("claude", "claude-restart").await,
        Some(first_session)
    );
    assert!(rejected.get_session_message("claude", "u3").await.is_none());
    assert!(
        rejected
            .lcm_load_raw_message("claude", "u3")
            .await
            .is_none()
    );
    drop(rejected);

    set_session_message_projection_failure(&project, false).await;

    let reopened = open_project_session_db(&project).await.unwrap();
    let suffix = ingest_source(&reopened, &ClaudeSource::with_home(&home), &project, None).await;
    assert_eq!(suffix.messages_upserted, 1);
    let final_offset = reopened.get_parse_offset(&path_key).await.unwrap();
    assert!(final_offset.byte_offset > first_offset.byte_offset);
    assert_eq!(
        final_offset.byte_offset,
        std::fs::metadata(&path).unwrap().len()
    );
    drop(reopened);

    let replay = open_project_session_db(&project).await.unwrap();
    let unchanged = ingest_source(&replay, &ClaudeSource::with_home(&home), &project, None).await;
    assert_eq!(unchanged.sessions_upserted, 0);
    assert_eq!(unchanged.messages_upserted, 0);
    assert_eq!(replay.get_parse_offset(&path_key).await, Some(final_offset));
    assert_eq!(replay.session_message_count().await.unwrap(), 3);
}

#[tokio::test]
async fn claude_restart_defers_a_partial_final_line() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let path = write_claude_transcript(&home, &project, "claude-partial");
    let path_key = path.to_string_lossy().to_string();
    let complete_len = std::fs::metadata(&path).unwrap().len();
    let partial = serde_json::json!({
        "type": "user",
        "cwd": project,
        "sessionId": "claude-partial",
        "uuid": "u3",
        "timestamp": "2026-01-01T00:00:10.000Z",
        "message": {"role": "user", "content": "Deferred Claude partial line."}
    })
    .to_string();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(partial.as_bytes())
        .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let first = ingest_source(&db, &ClaudeSource::with_home(&home), &project, None).await;
    assert_eq!(first.messages_upserted, 2);
    let committed_offset = db.get_parse_offset(&path_key).await.unwrap();
    assert_eq!(committed_offset.byte_offset, complete_len);
    drop(db);

    let reopened = open_project_session_db(&project).await.unwrap();
    let still_partial =
        ingest_source(&reopened, &ClaudeSource::with_home(&home), &project, None).await;
    assert_eq!(still_partial.messages_upserted, 0);
    assert_eq!(
        reopened.get_parse_offset(&path_key).await,
        Some(committed_offset)
    );

    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"\n")
        .unwrap();
    let completed = ingest_source(&reopened, &ClaudeSource::with_home(&home), &project, None).await;
    assert_eq!(completed.messages_upserted, 1);
    assert_eq!(reopened.session_message_count().await.unwrap(), 3);
    assert_eq!(
        reopened
            .get_parse_offset(&path_key)
            .await
            .unwrap()
            .byte_offset,
        std::fs::metadata(path).unwrap().len()
    );
}

#[tokio::test]
async fn cline_content_hash_cursor_survives_restart_and_incomplete_rewrite() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let history_path = write_task(
        &vscode_storage_root(&home, "saoudrizwan.claude-dev"),
        &project,
        "cline-restart",
    );
    let source = ClineLikeSource::cline_with_home(&home);

    let db = open_project_session_db(&project).await.unwrap();
    let first = ingest_source(&db, &source, &project, None).await;
    assert_eq!(first.messages_upserted, 2);
    let offset = parse_offset_for_task_history(&db, &project, &history_path)
        .await
        .unwrap();
    drop(db);

    let reopened = open_project_session_db(&project).await.unwrap();
    let replay = ingest_source(&reopened, &source, &project, None).await;
    assert_eq!(replay.sessions_upserted, 0);
    assert_eq!(replay.messages_upserted, 0);
    assert_eq!(
        parse_offset_for_task_history(&reopened, &project, &history_path).await,
        Some(offset)
    );
    assert_eq!(
        reopened
            .search_session_messages("cline", None, "billing pipeline", 10)
            .await
            .len(),
        2
    );
    drop(reopened);

    std::fs::write(
        &history_path,
        r#"[{"role":"user","content":"Incomplete Cline rewrite."}"#,
    )
    .unwrap();
    let incomplete = open_project_session_db(&project).await.unwrap();
    let deferred = ingest_source(&incomplete, &source, &project, None).await;
    assert_eq!(deferred.messages_upserted, 0);
    assert_eq!(incomplete.session_message_count().await.unwrap(), 2);
    let incomplete_offset = parse_offset_for_task_history(&incomplete, &project, &history_path)
        .await
        .unwrap();
    assert_ne!(incomplete_offset, offset);
    drop(incomplete);

    std::fs::write(
        &history_path,
        serde_json::to_string_pretty(&serde_json::json!([
            {
                "role": "user",
                "content": "Investigate the billing pipeline regression",
                "ts": 1_800_000_000_i64
            },
            {
                "role": "assistant",
                "content": "The billing pipeline regression is fixed.",
                "ts": 1_800_000_010_i64
            },
            {
                "role": "user",
                "content": "Verify the completed Cline rewrite.",
                "ts": 1_800_000_020_i64
            }
        ]))
        .unwrap(),
    )
    .unwrap();
    let completed = open_project_session_db(&project).await.unwrap();
    let recovered = ingest_source(&completed, &source, &project, None).await;
    assert_eq!(recovered.messages_upserted, 3);
    assert_eq!(completed.session_message_count().await.unwrap(), 3);
    assert_eq!(
        completed
            .search_session_messages("cline", None, "completed Cline rewrite", 10)
            .await
            .len(),
        1
    );
    assert_ne!(
        parse_offset_for_task_history(&completed, &project, &history_path)
            .await
            .unwrap(),
        incomplete_offset
    );
}

#[tokio::test]
async fn cursor_restart_is_idempotent_and_ingests_only_the_appended_suffix() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let transcript_path = tmp.path().join("cursor-restart.jsonl");
    std::fs::write(
        &transcript_path,
        "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"First restart message.\"}]}}\n",
    )
    .unwrap();
    let event = serde_json::json!({
        "session_id": "cursor-restart",
        "transcript_path": transcript_path,
        "workspace_roots": [project]
    });

    let db = open_project_session_db(&project).await.unwrap();
    let first = ingest_cursor_transcript_event(&event.to_string(), &db).await;
    assert_eq!(first.messages_upserted, 1);
    let first_offset = db
        .get_parse_offset(transcript_path.to_string_lossy().as_ref())
        .await
        .unwrap();
    drop(db);

    let reopened = open_project_session_db(&project).await.unwrap();
    let unchanged = ingest_cursor_transcript_event(&event.to_string(), &reopened).await;
    assert_eq!(unchanged.sessions_upserted, 0);
    assert_eq!(unchanged.messages_upserted, 0);

    let mut transcript = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript_path)
        .unwrap();
    transcript
        .write_all(
            b"{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Second restart message.\"}]}}\n",
        )
        .unwrap();
    drop(transcript);
    drop(reopened);

    let catchup = open_project_session_db(&project).await.unwrap();
    let suffix = ingest_cursor_transcript_event(&event.to_string(), &catchup).await;
    assert_eq!(suffix.messages_upserted, 1);
    let final_offset = catchup
        .get_parse_offset(transcript_path.to_string_lossy().as_ref())
        .await
        .unwrap();
    assert!(final_offset.byte_offset > first_offset.byte_offset);
    assert_eq!(
        catchup
            .search_session_messages("cursor", None, "restart message", 10)
            .await
            .len(),
        2
    );
}

#[tokio::test]
async fn cursor_restart_defers_a_partial_final_line() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let transcript_path = tmp.path().join("cursor-partial.jsonl");
    let complete = "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Complete partial test line.\"}]}}\n";
    let partial = "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Deferred partial test line.\"}]}}";
    std::fs::write(&transcript_path, format!("{complete}{partial}")).unwrap();
    let event = serde_json::json!({
        "session_id": "cursor-partial",
        "transcript_path": transcript_path,
        "workspace_roots": [project]
    });

    let db = open_project_session_db(&project).await.unwrap();
    let first = ingest_cursor_transcript_event(&event.to_string(), &db).await;
    assert_eq!(first.messages_upserted, 1);
    let committed_offset = db
        .get_parse_offset(transcript_path.to_string_lossy().as_ref())
        .await
        .unwrap();
    assert_eq!(committed_offset.byte_offset, complete.len() as u64);
    drop(db);

    let reopened = open_project_session_db(&project).await.unwrap();
    let still_partial = ingest_cursor_transcript_event(&event.to_string(), &reopened).await;
    assert_eq!(still_partial.messages_upserted, 0);
    assert_eq!(
        reopened
            .get_parse_offset(transcript_path.to_string_lossy().as_ref())
            .await,
        Some(committed_offset)
    );

    let mut transcript = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript_path)
        .unwrap();
    transcript.write_all(b"\n").unwrap();
    drop(transcript);

    let completed = ingest_cursor_transcript_event(&event.to_string(), &reopened).await;
    assert_eq!(completed.messages_upserted, 1);
    assert_eq!(
        reopened
            .search_session_messages("cursor", None, "partial test line", 10)
            .await
            .len(),
        2
    );
}
