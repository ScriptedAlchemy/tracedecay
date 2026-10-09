//! A store the v1.0.0-beta.66 daemon left behind (LCM schema 14, message
//! projector v6) holds block-array messages as JSON and session-temporal
//! effects immutably bound to that rendering, so this tree refuses it with the
//! typed profile reset instead of serving or rebuilding its rows, and a reset
//! store serves both the Cursor and the Claude message.

use std::path::Path;

use tempfile::TempDir;
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_lcm::LCM_SCHEMA_VERSION;
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_sessions::SessionProvider;
use tracedecay_sessions::admission::HostAdmissionScope;
use tracedecay_sessions::runtime::hosts::cursor::ingest_cursor_transcript_event;
use tracedecay_sessions::runtime::with_transcript_source_profile;
use tracedecay_store::SESSION_MESSAGE_PROJECTOR_VERSION;

use crate::restart_atomicity::{
    ProjectSessionTestRuntime, ingest_global_sources_for_provider, open_project_session_db,
};
use crate::support::init_project_at;

const SHIPPED_LCM_SCHEMA_VERSION: i64 = 14;
const SHIPPED_PROJECTOR_VERSION: &str = "claude-session-message-v6";
const CURSOR_TEXT: &str = "cursornow beta: reply";
const CURSOR_SHIPPED_TEXT: &str = r#"[{"text":"cursornow beta: reply","type":"text"}]"#;
const CLAUDE_TEXT: &str = "claudeprobe: investigate the billing pipeline regression";

async fn search_texts(db: &ProjectSessionTestRuntime, provider: &str, query: &str) -> Vec<String> {
    let mut texts = db
        .search_session_messages(provider, None, query, 10)
        .await
        .into_iter()
        .map(|hit| hit.message.text)
        .collect::<Vec<_>>();
    texts.sort();
    texts
}

async fn ingest_both(db: &ProjectSessionTestRuntime, home: &Path, project: &Path, cursor: &Path) {
    let event = serde_json::json!({
        "session_id": "cursor-session",
        "transcript_path": cursor,
        "workspace_roots": [project]
    });
    with_transcript_source_profile(
        db.transcript_source_profile(),
        ingest_cursor_transcript_event(
            &event.to_string(),
            &db.runtime().facade(),
            db.project_id().clone(),
        ),
    )
    .await;
    ingest_global_sources_for_provider(home, db, project, Some(SessionProvider::Claude)).await;
}

/// Leaves the store exactly as the shipped daemon would have: its LCM schema
/// marker at 14, its projection rows owned by projector v6, and the Cursor
/// rows holding the block array's JSON.
fn shape_store_as_shipped(db_path: &Path) {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.execute(
        "UPDATE session_schema_migrations SET version = ?1 WHERE name = 'lcm'",
        [SHIPPED_LCM_SCHEMA_VERSION],
    )
    .unwrap();
    for table in [
        "observation_projection_provenance",
        "observation_projection_checkpoints",
    ] {
        conn.execute(
            &format!("UPDATE {table} SET projector_version = ?1 WHERE projector_version = ?2"),
            rusqlite::params![SHIPPED_PROJECTOR_VERSION, SESSION_MESSAGE_PROJECTOR_VERSION],
        )
        .unwrap();
    }
    let rewritten = conn
        .execute(
            "UPDATE lcm_raw_messages
             SET content = '[{\"text\":\"' || content || '\",\"type\":\"text\"}]'
             WHERE provider = 'cursor'",
            (),
        )
        .unwrap();
    assert_eq!(rewritten, 2, "both Cursor rows must exist to be shaped");
    let reply: String = conn
        .query_row(
            "SELECT content FROM lcm_raw_messages WHERE provider = 'cursor' AND role = 'assistant'",
            (),
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reply, CURSOR_SHIPPED_TEXT);
}

/// What `tracedecay wipe --stale --yes` does to a refused store: removes it
/// so the next open recreates it empty.
fn reset_store(db_path: &Path) {
    std::fs::remove_file(db_path).unwrap();
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = db_path.as_os_str().to_owned();
        sidecar.push(suffix);
        match std::fs::remove_file(sidecar) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("remove closed store {suffix}: {error}"),
        }
    }
}

#[tokio::test]
async fn shipped_store_is_reset_required_and_a_reset_store_serves_both_messages() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    init_project_at(&project);

    let cursor_transcript = tmp.path().join("cursor-session.jsonl");
    std::fs::write(
        &cursor_transcript,
        format!(
            "{}\n{}\n",
            serde_json::json!({"role": "user", "message": {"content": [{"type": "text", "text": "cursornow alpha: question"}]}}),
            serde_json::json!({"role": "assistant", "message": {"content": [{"type": "text", "text": CURSOR_TEXT}]}}),
        ),
    )
    .unwrap();
    let claude_transcripts = home.join(".claude/projects/-project");
    std::fs::create_dir_all(&claude_transcripts).unwrap();
    std::fs::write(
        claude_transcripts.join("claude-sess.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({
                "type": "user", "cwd": project, "gitBranch": "main", "sessionId": "claude-sess",
                "uuid": "u1", "timestamp": "2026-01-01T00:00:00.000Z",
                "message": {"role": "user", "content": CLAUDE_TEXT}
            })
        ),
    )
    .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let profile_root = db.runtime().profile_root_for_test().to_path_buf();
    let project_id = db.project_id().clone();
    let db_path = db
        .runtime()
        .database_path(HostAdmissionScope::Project)
        .unwrap()
        .to_path_buf();
    ingest_both(&db, &home, &project, &cursor_transcript).await;
    assert_eq!(search_texts(&db, "cursor", "beta").await, [CURSOR_TEXT]);
    assert_eq!(
        search_texts(&db, "claude", "claudeprobe").await,
        [CLAUDE_TEXT]
    );
    db.shutdown()
        .await
        .expect("close stores before shaping the shipped schema");
    shape_store_as_shipped(&db_path);

    // Retain the profile owner across the refused project mount so its
    // partially opened workers can be joined before the offline reset.
    let profile = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .expect("mount the current profile around the shipped project store");
    let refused = HostAdmissionTestRuntimeV1::project(&profile_root, &project, project_id.clone())
        .await
        .err()
        .expect("a shipped store must not be admitted or served");
    assert!(
        matches!(
            refused,
            TraceDecayError::ProfileResetRequired {
                component: "LCM",
                found_version: Some(SHIPPED_LCM_SCHEMA_VERSION),
                required_version: LCM_SCHEMA_VERSION,
            }
        ),
        "{refused:?}"
    );
    let untouched: i64 = rusqlite::Connection::open(&db_path)
        .unwrap()
        .query_row(
            "SELECT version FROM session_schema_migrations WHERE name = 'lcm'",
            (),
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        untouched, SHIPPED_LCM_SCHEMA_VERSION,
        "a refused store is left for the operator's reset, never rewritten"
    );

    profile
        .shutdown()
        .await
        .expect("close refused project mount before reset");
    reset_store(&db_path);
    let reset = open_project_session_db(&project).await.unwrap();
    ingest_both(&reset, &home, &project, &cursor_transcript).await;
    assert_eq!(search_texts(&reset, "cursor", "beta").await, [CURSOR_TEXT]);
    assert_eq!(
        search_texts(&reset, "claude", "claudeprobe").await,
        [CLAUDE_TEXT]
    );
}
