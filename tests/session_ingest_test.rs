use std::fs::{self, OpenOptions};
use std::io::Write;

use tempfile::TempDir;
use tokensave::global_db::GlobalDb;
use tokensave::sessions::{ingest_sessions_from_roots, SessionIngestProvider, SessionIngestRoots};

async fn open_isolated_db(tmp: &TempDir) -> GlobalDb {
    let db_path = tmp.path().join(".tokensave").join("global.db");
    GlobalDb::open_at(&db_path).await.expect("global db open")
}

fn write_jsonl(path: &std::path::Path, lines: &[&str]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture parent");
    }
    fs::write(path, format!("{}\n", lines.join("\n"))).expect("write fixture");
}

#[tokio::test]
async fn cursor_jsonl_ingests_messages_for_fts_search() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    let cursor_home = tmp.path().join("cursor-home");
    let transcript =
        cursor_home.join(".cursor/projects/project-slug/agent-transcripts/convo-1/convo-1.jsonl");
    write_jsonl(
        &transcript,
        &[
            r#"{"role":"user","message":{"content":[{"type":"text","text":"Please explain needle-indexing for local transcripts."}]}}"#,
            r#"{"role":"assistant","message":{"content":[{"type":"text","text":"Needle indexing works by flattening message text."},{"type":"tool_use","name":"tokensave_search","input":{"query":"needle indexing"}}]},"model":"gpt-5.5"}"#,
            r#"{"unknown":true}"#,
        ],
    );

    let roots = SessionIngestRoots {
        cursor_home,
        codex_home: tmp.path().join("codex-home"),
    };
    let stats = ingest_sessions_from_roots(&db, SessionIngestProvider::Cursor, &roots).await;

    assert_eq!(stats.files_seen, 1);
    assert_eq!(stats.messages_inserted, 2);
    assert_eq!(stats.malformed_lines, 0);

    let results = db
        .search_session_messages("cursor", Some("project-slug"), "needle-indexing", 10)
        .await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].session.session_id, "convo-1");
    assert_eq!(results[0].message.role, "user");
}

#[tokio::test]
async fn codex_rollout_ingests_messages_and_reports_exact_token_usage() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    let codex_home = tmp.path().join("codex-home");
    let rollout = codex_home.join(".codex/sessions/2026/06/07/rollout-abc.jsonl");
    write_jsonl(
        &rollout,
        &[
            r#"{"type":"session_meta","payload":{"id":"codex-session-1","timestamp":"2026-06-07T08:00:00Z"}}"#,
            r#"{"type":"turn_context","payload":{"cwd":"/work/project","model":"gpt-5.4-medium"}}"#,
            r#"{"type":"response_item","payload":{"item":{"id":"item-1","type":"message","role":"assistant","content":[{"type":"output_text","text":"Codex found the rollout needle."}]}}}"#,
            r#"{"type":"event_msg","msg":{"type":"token_count","info":{"last_token_usage":{"input_tokens":1200,"cached_input_tokens":300,"output_tokens":80}}}}"#,
        ],
    );

    let roots = SessionIngestRoots {
        cursor_home: tmp.path().join("cursor-home"),
        codex_home,
    };
    let stats = ingest_sessions_from_roots(&db, SessionIngestProvider::Codex, &roots).await;

    assert_eq!(stats.files_seen, 1);
    assert_eq!(stats.messages_inserted, 1);
    assert_eq!(stats.token_usages.len(), 1);
    assert_eq!(stats.token_usages[0].input_tokens, 1200);
    assert_eq!(stats.token_usages[0].cache_read_tokens, 300);
    assert_eq!(stats.token_usages[0].output_tokens, 80);

    let results = db
        .search_session_messages("codex", Some("/work/project"), "rollout needle", 10)
        .await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].session.session_id, "codex-session-1");
    assert_eq!(results[0].message.model.as_deref(), Some("gpt-5.4-medium"));
}

#[tokio::test]
async fn offset_resume_only_ingests_appended_lines() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    let cursor_home = tmp.path().join("cursor-home");
    let transcript =
        cursor_home.join(".cursor/projects/project-slug/agent-transcripts/convo-2/convo-2.jsonl");
    write_jsonl(
        &transcript,
        &[
            r#"{"role":"user","message":{"content":[{"type":"text","text":"first searchable marker"}]}}"#,
        ],
    );

    let roots = SessionIngestRoots {
        cursor_home,
        codex_home: tmp.path().join("codex-home"),
    };
    let first = ingest_sessions_from_roots(&db, SessionIngestProvider::Cursor, &roots).await;
    assert_eq!(first.messages_inserted, 1);

    let mut file = OpenOptions::new()
        .append(true)
        .open(&transcript)
        .expect("open transcript for append");
    writeln!(
        file,
        r#"{{"role":"assistant","message":{{"content":[{{"type":"text","text":"second appended marker"}}]}}}}"#
    )
    .expect("append line");

    let second = ingest_sessions_from_roots(&db, SessionIngestProvider::Cursor, &roots).await;
    assert_eq!(second.messages_inserted, 1);

    assert_eq!(
        db.search_session_messages("cursor", Some("project-slug"), "first searchable", 10)
            .await
            .len(),
        1
    );
    assert_eq!(
        db.search_session_messages("cursor", Some("project-slug"), "second appended", 10)
            .await
            .len(),
        1
    );

    let offset_key = format!("cursor:{}", transcript.to_string_lossy().replace('\\', "/"));
    assert!(db.get_parse_offset(&offset_key).await.is_some());
}

#[tokio::test]
async fn malformed_and_unknown_records_fail_open() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    let codex_home = tmp.path().join("codex-home");
    let rollout = codex_home.join(".codex/sessions/2026/06/07/rollout-bad.jsonl");
    write_jsonl(
        &rollout,
        &[
            "not json at all",
            r#"{"type":"unknown","payload":{"anything":true}}"#,
            r#"{"type":"response_item","payload":{"item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"still searchable after bad records"}]}}}"#,
        ],
    );

    let roots = SessionIngestRoots {
        cursor_home: tmp.path().join("cursor-home"),
        codex_home,
    };
    let stats = ingest_sessions_from_roots(&db, SessionIngestProvider::Codex, &roots).await;

    assert_eq!(stats.files_seen, 1);
    assert_eq!(stats.messages_inserted, 1);
    assert_eq!(stats.malformed_lines, 1);
    assert_eq!(
        db.search_session_messages("codex", None, "still searchable", 10)
            .await
            .len(),
        1
    );
}
