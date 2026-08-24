//! Fixture-based tests for Cursor composer ingestion
//! ([`tracedecay::sessions::cursor_composer`]).
//!
//! Each test builds a small synthetic `state.vscdb` (and, for the DAG test, a
//! `store.db`) with libsql, then drives the read-only composer sweep and
//! asserts the mapped rows, JSONL dedupe, incremental watermark, and malformed
//! tolerance. No real Cursor data is touched.

use std::collections::HashSet;
use std::path::Path;

use tempfile::TempDir;
use tracedecay::sessions::cursor::open_project_session_db;
use tracedecay::sessions::cursor::{CursorSweepSource, cursor_project_slug};
use tracedecay::sessions::cursor_composer::CursorComposerSource;
use tracedecay::sessions::source::ingest_source;

use crate::support::init_project;

const CAP: usize = 256;

/// Write a `state.vscdb` with the `cursorDiskKV` schema at the real Cursor
/// path under `home`, populated with the given `(key, value)` rows.
async fn write_state_vscdb(home: &Path, rows: &[(String, String)]) {
    let dir = home
        .join(".config")
        .join("Cursor")
        .join("User")
        .join("globalStorage");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("state.vscdb");
    let db = libsql::Builder::new_local(&path).build().await.unwrap();
    let conn = db.connect().unwrap();
    // DELETE journalling keeps everything in the main file so the immutable
    // read path sees it without a -wal sidecar.
    conn.execute_batch(
        "PRAGMA journal_mode=DELETE;\n\
         CREATE TABLE IF NOT EXISTS cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);",
    )
    .await
    .unwrap();
    for (key, value) in rows {
        conn.execute(
            "INSERT OR REPLACE INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
            libsql::params![key.clone(), value.clone()],
        )
        .await
        .unwrap();
    }
    drop(conn);
    drop(db);
}

fn envelope(composer_id: &str, project: &Path, header_bubble_ids: &[&str]) -> serde_json::Value {
    let headers: Vec<serde_json::Value> = header_bubble_ids
        .iter()
        .enumerate()
        .map(
            |(i, id)| serde_json::json!({ "bubbleId": id, "type": if i % 2 == 0 { 1 } else { 2 } }),
        )
        .collect();
    serde_json::json!({
        "composerId": composer_id,
        "name": "Composer session",
        "createdAt": 1_700_000_000_000i64,
        "lastUpdatedAt": serde_json::Value::Null,
        "unifiedMode": "agent",
        "modelConfig": { "modelName": "claude-opus-4-8" },
        "workspaceIdentifier": {
            "id": "ws-hash-1",
            "uri": { "fsPath": project.to_string_lossy(), "path": project.to_string_lossy() }
        },
        "todos": [
            { "id": "t1", "content": "First todo", "status": "completed" },
            { "id": "t2", "content": "Second todo", "status": "pending" }
        ],
        "fullConversationHeadersOnly": headers,
    })
}

fn kv(key: &str, value: &serde_json::Value) -> (String, String) {
    (key.to_string(), value.to_string())
}

#[cfg(unix)]
#[tokio::test]
async fn composer_unknown_project_membership_defers_persistence_and_offset() {
    const CHILD_ENV: &str = "TRACEDECAY_COMPOSER_UNKNOWN_MEMBERSHIP_CHILD";
    if std::env::var_os(CHILD_ENV).is_some() {
        let tmp = TempDir::new().unwrap();
        let project = init_project(&tmp);
        let nested = project.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let home = tmp.path().join("home");
        let env = envelope("unknown-composer", &nested, &["b-user"]);
        let bubble = serde_json::json!({"type": 1, "text": "defer this session"});
        write_state_vscdb(
            &home,
            &[
                kv("composerData:unknown-composer", &env),
                kv("bubbleId:unknown-composer:b-user", &bubble),
            ],
        )
        .await;

        let db = open_project_session_db(&project).await.unwrap();
        let outcome = CursorComposerSource::with_home(&home)
            .ingest_user(&db, &[project], CAP)
            .await;
        assert_eq!(outcome.sessions_upserted, 0);
        assert!(outcome.owned_session_ids.is_empty());
        assert!(db.get_session("cursor", "unknown-composer").await.is_none());
        assert!(
            db.get_parse_offset("cursor-composer:unknown-composer")
                .await
                .is_none()
        );
        return;
    }

    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let tmp = TempDir::new().unwrap();
    let fake_git = tmp.path().join("git-timeout");
    std::fs::write(&fake_git, "#!/bin/sh\nexec /bin/sleep 3\n").unwrap();
    std::fs::set_permissions(&fake_git, std::fs::Permissions::from_mode(0o755)).unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .arg("cursor_composer::composer_unknown_project_membership_defers_persistence_and_offset")
        .arg("--exact")
        .env(CHILD_ENV, "1")
        .env("GIT", fake_git)
        .env(
            "GIT_DIR",
            "/nonexistent/tracedecay-composer-timeout-git-dir",
        )
        .status()
        .unwrap();
    assert!(
        status.success(),
        "child must defer unknown project membership"
    );
}

/// Envelope + user bubble + a rich assistant bubble (text, thinking, tool call,
/// token counts) + todos map to the expected provider-neutral rows.
#[tokio::test]
async fn composer_envelope_and_bubbles_ingest_rows() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let home = tmp.path().join("home");

    let user_bubble = serde_json::json!({
        "type": 1,
        "text": "Please refactor the widget module for clarity."
    });
    let assistant_bubble = serde_json::json!({
        "type": 2,
        "text": "Done refactoring the widget module.",
        "thinking": { "signature": "sig", "text": "Considering the widget invariants carefully." },
        "toolFormerData": {
            "tool": 15,
            "name": "edit_file",
            "status": "completed",
            "toolCallId": "call-1",
            "params": "{\"path\":\"widget.rs\"}",
            "result": "{\"ok\":true}"
        },
        "tokenCount": { "inputTokens": 1200, "outputTokens": 340 },
        "pullRequests": [ { "url": "https://example.invalid/pr/7", "title": "Refactor widget" } ]
    });
    let env = envelope("comp-1", &project, &["b-user", "b-asst"]);
    let rows = vec![
        kv("composerData:comp-1", &env),
        kv("bubbleId:comp-1:b-user", &user_bubble),
        kv("bubbleId:comp-1:b-asst", &assistant_bubble),
    ];
    write_state_vscdb(&home, &rows).await;

    let db = open_project_session_db(&project).await.unwrap();
    let outcome = CursorComposerSource::with_home(&home)
        .ingest(&db, &project, CAP)
        .await;

    assert_eq!(
        outcome.sessions_upserted, 1,
        "one composer session ingested"
    );
    assert!(outcome.owned_session_ids.contains("comp-1"));

    let session = db
        .get_session("cursor", "comp-1")
        .await
        .expect("composer session stored");
    assert_eq!(session.project_path, project.to_string_lossy());
    assert_eq!(session.title.as_deref(), Some("Composer session"));

    // Message row.
    let message = db
        .get_session_message("cursor", "comp-1:b-asst")
        .await
        .expect("assistant message row");
    assert_eq!(message.kind.as_deref(), Some("message"));
    assert_eq!(message.model.as_deref(), Some("claude-opus-4-8"));
    let meta: serde_json::Value =
        serde_json::from_str(message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(meta["usage"]["input_tokens"], 1200);
    assert_eq!(meta["usage"]["output_tokens"], 340);

    // Reasoning row.
    let reasoning = db
        .get_session_message("cursor", "comp-1:b-asst:thinking")
        .await
        .expect("reasoning row");
    assert_eq!(reasoning.kind.as_deref(), Some("reasoning"));
    assert!(reasoning.text.contains("widget invariants"));

    // Tool call row -> file_edit for an edit tool.
    let tool = db
        .get_session_message("cursor", "comp-1:b-asst:tool")
        .await
        .expect("tool row");
    assert_eq!(tool.kind.as_deref(), Some("file_edit"));
    assert_eq!(tool.tool_names.as_deref(), Some("edit_file"));

    // PR link row.
    let pr = db
        .get_session_message("cursor", "comp-1:b-asst:pr:0")
        .await
        .expect("pr_link row");
    assert_eq!(pr.kind.as_deref(), Some("pr_link"));
    assert!(pr.text.contains("example.invalid/pr/7"));

    // Plan row from the envelope todos.
    let plan = db
        .get_session_message("cursor", "comp-1:plan")
        .await
        .expect("plan row");
    assert_eq!(plan.kind.as_deref(), Some("plan"));
    let plan_meta: serde_json::Value =
        serde_json::from_str(plan.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(plan_meta["todos"][0]["id"], "t1");
    assert_eq!(plan_meta["todos"][1]["status"], "pending");
}

/// The JSONL sweep skips any session id owned by the composer store, so the two
/// Cursor sources never double-ingest the same session.
#[tokio::test]
async fn composer_owned_session_dedupes_jsonl_sweep() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let home = tmp.path().join("home");

    let env = envelope("comp-1", &project, &["b-user"]);
    let user_bubble = serde_json::json!({ "type": 1, "text": "Shared session prompt." });
    write_state_vscdb(
        &home,
        &[
            kv("composerData:comp-1", &env),
            kv("bubbleId:comp-1:b-user", &user_bubble),
        ],
    )
    .await;

    // A JSONL transcript named after the same session id (the ~94% overlap).
    let slug = cursor_project_slug(&project).unwrap();
    let transcripts_dir = home
        .join(".cursor")
        .join("projects")
        .join(slug)
        .join("agent-transcripts");
    std::fs::create_dir_all(&transcripts_dir).unwrap();
    std::fs::write(
        transcripts_dir.join("comp-1.jsonl"),
        "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"JSONL copy of the prompt.\"}]}}\n",
    )
    .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let outcome = CursorComposerSource::with_home(&home)
        .ingest(&db, &project, CAP)
        .await;
    assert!(outcome.owned_session_ids.contains("comp-1"));

    // JSONL sweep with the composer-owned skip set does not touch comp-1.
    let owned: HashSet<String> = outcome.owned_session_ids.clone();
    let skipped_sweep = CursorSweepSource::with_home(&home).with_skip_session_ids(owned);
    let skipped = ingest_source(&db, &skipped_sweep, &project, None).await;
    assert_eq!(
        skipped.messages_upserted, 0,
        "owned session must be skipped"
    );

    // Control: without the skip set the same JSONL file would ingest.
    let db2 = open_project_session_db(&project).await.unwrap();
    let _ = CursorComposerSource::with_home(&home)
        .ingest(&db2, &project, CAP)
        .await;
    let plain_sweep = CursorSweepSource::with_home(&home);
    let plain = ingest_source(&db2, &plain_sweep, &project, None).await;
    assert_eq!(
        plain.messages_upserted, 1,
        "without the skip set the JSONL copy ingests"
    );
}

/// A second pass over an unchanged store is a no-op; growing the bubble count
/// re-ingests.
#[tokio::test]
async fn composer_watermark_skips_unchanged_and_reingests_growth() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let home = tmp.path().join("home");

    let env = envelope("comp-1", &project, &["b1"]);
    let b1 = serde_json::json!({ "type": 1, "text": "First prompt." });
    write_state_vscdb(
        &home,
        &[
            kv("composerData:comp-1", &env),
            kv("bubbleId:comp-1:b1", &b1),
        ],
    )
    .await;

    let db = open_project_session_db(&project).await.unwrap();
    let source = CursorComposerSource::with_home(&home);
    let first = source.ingest(&db, &project, CAP).await;
    assert_eq!(first.sessions_upserted, 1);

    let second = source.ingest(&db, &project, CAP).await;
    assert_eq!(
        second.sessions_upserted, 0,
        "unchanged session must skip without re-upserting"
    );
    // Still owned so JSONL keeps standing down even when skipped by watermark.
    assert!(second.owned_session_ids.contains("comp-1"));

    // Grow the conversation: append a second bubble + header.
    let grown = envelope("comp-1", &project, &["b1", "b2"]);
    let b2 = serde_json::json!({ "type": 2, "text": "Second turn reply." });
    write_state_vscdb(
        &home,
        &[
            kv("composerData:comp-1", &grown),
            kv("bubbleId:comp-1:b1", &b1),
            kv("bubbleId:comp-1:b2", &b2),
        ],
    )
    .await;
    let third = source.ingest(&db, &project, CAP).await;
    assert_eq!(third.sessions_upserted, 1, "growth re-ingests");
    let reply = db
        .get_session_message("cursor", "comp-1:b2")
        .await
        .expect("new bubble row");
    assert!(reply.text.contains("Second turn reply"));
}

/// Malformed envelopes are tolerated, and a per-session `store.db` blob DAG is
/// walked into ordered rows.
#[tokio::test]
async fn composer_tolerates_malformed_and_reads_store_db() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let home = tmp.path().join("home");

    // Envelope providing the ws-hash -> project mapping the store.db needs,
    // plus a deliberately malformed composerData row that must not panic.
    let env = envelope("comp-1", &project, &["b1"]);
    let b1 = serde_json::json!({ "type": 1, "text": "Envelope prompt." });
    write_state_vscdb(
        &home,
        &[
            kv("composerData:comp-1", &env),
            kv("bubbleId:comp-1:b1", &b1),
            (
                "composerData:broken".to_string(),
                "{not valid json".to_string(),
            ),
        ],
    )
    .await;

    // Build a store.db under ~/.cursor/chats/<ws-hash>/<agentId>/store.db.
    let agent_dir = home
        .join(".cursor")
        .join("chats")
        .join("ws-hash-1")
        .join("agent-1");
    std::fs::create_dir_all(&agent_dir).unwrap();
    write_store_db(&agent_dir.join("store.db")).await;

    let db = open_project_session_db(&project).await.unwrap();
    let outcome = CursorComposerSource::with_home(&home)
        .ingest(&db, &project, CAP)
        .await;

    // Both the composer envelope session and the store.db chat session ingested.
    assert!(outcome.owned_session_ids.contains("comp-1"));
    assert!(outcome.owned_session_ids.contains("cursor-chat:agent-1"));
    assert!(
        outcome.sessions_upserted >= 2,
        "envelope + store.db sessions, got {}",
        outcome.sessions_upserted
    );

    // The DAG walk orders system(0), user(1), assistant(2); the junk blob is
    // tolerated and skipped.
    let store_user = db
        .get_session_message("cursor", "cursor-chat:agent-1:1")
        .await
        .expect("store.db user message row");
    assert_eq!(store_user.role, "user");
    assert!(store_user.text.contains("hello from store"));
    let store_session = db
        .get_session("cursor", "cursor-chat:agent-1")
        .await
        .expect("store.db chat session stored");
    assert_eq!(store_session.project_path, project.to_string_lossy());
}

/// Build a `store.db` whose root node blob references three JSON message leaves
/// in order, plus one non-JSON blob to exercise malformed-blob tolerance.
async fn write_store_db(path: &Path) {
    let sys_id = "aa".repeat(32);
    let user_id = "bb".repeat(32);
    let asst_id = "cc".repeat(32);
    let junk_id = "dd".repeat(32);
    let root_id = "ee".repeat(32);

    let sys = serde_json::json!({ "role": "system", "content": "system preamble" });
    let user = serde_json::json!({ "role": "user", "content": "hello from store" });
    let asst = serde_json::json!({
        "role": "assistant",
        "content": [ { "type": "text", "text": "reply from store" } ]
    });

    // Root node blob: protobuf field-1 length-delimited 32-byte child refs.
    let mut root = Vec::new();
    for id in [&sys_id, &user_id, &asst_id] {
        root.push(0x0a);
        root.push(0x20);
        root.extend_from_slice(&hex_decode(id));
    }

    let meta = serde_json::json!({
        "agentId": "agent-1",
        "latestRootBlobId": root_id,
        "name": "Store chat",
        "mode": "agent",
        "createdAt": 1_700_000_100_000i64,
    });
    let meta_hex = hex_encode(meta.to_string().as_bytes());

    let db = libsql::Builder::new_local(path).build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode=DELETE;\n\
         CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);\n\
         CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB);",
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO meta (key, value) VALUES ('0', ?1)",
        libsql::params![meta_hex],
    )
    .await
    .unwrap();
    let blobs: Vec<(String, Vec<u8>)> = vec![
        (sys_id, sys.to_string().into_bytes()),
        (user_id, user.to_string().into_bytes()),
        (asst_id, asst.to_string().into_bytes()),
        (junk_id, vec![0x00, 0x01, 0x02, 0xff]),
        (root_id, root),
    ];
    for (id, data) in blobs {
        conn.execute(
            "INSERT INTO blobs (id, data) VALUES (?1, ?2)",
            libsql::params![id, libsql::Value::Blob(data)],
        )
        .await
        .unwrap();
    }
    drop(conn);
    drop(db);
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn hex_decode(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

/// Manual live smoke check against the real ~21 GB `state.vscdb`. Ignored by
/// default; run with:
/// `CURSOR_SMOKE_PROJECT=/abs/project cargo nextest run --run-ignored all \
///   -E 'test(live_smoke_real_state_vscdb)' --no-capture`.
/// Self-skips when `$HOME/.config/Cursor/.../state.vscdb` or the project env
/// var are absent, so it is a no-op on machines without real Cursor data.
#[tokio::test]
#[ignore]
async fn live_smoke_real_state_vscdb() {
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        eprintln!("LIVE SMOKE skipped: no HOME");
        return;
    };
    let state_db = home.join(".config/Cursor/User/globalStorage/state.vscdb");
    let Some(project) = std::env::var_os("CURSOR_SMOKE_PROJECT").map(std::path::PathBuf::from)
    else {
        eprintln!("LIVE SMOKE skipped: set CURSOR_SMOKE_PROJECT to an absolute project path");
        return;
    };
    if !state_db.is_file() {
        eprintln!("LIVE SMOKE skipped: {} not found", state_db.display());
        return;
    }
    let tmp = TempDir::new().unwrap();
    let db = tracedecay::global_db::GlobalDb::open_at(&tmp.path().join("sessions.db"))
        .await
        .unwrap();

    let start = std::time::Instant::now();
    let outcome = CursorComposerSource::with_home(&home)
        .ingest(&db, &project, 50)
        .await;
    let elapsed = start.elapsed();
    eprintln!(
        "LIVE SMOKE: sessions={} messages={} owned={} elapsed={:?}",
        outcome.sessions_upserted,
        outcome.messages_upserted,
        outcome.owned_session_ids.len(),
        elapsed
    );
    assert!(!outcome.owned_session_ids.is_empty());
    assert!(outcome.messages_upserted > 0);
}
