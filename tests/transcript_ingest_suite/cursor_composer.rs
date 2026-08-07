//! Fixture-based tests for Cursor composer ingestion
//! ([`tracedecay::sessions::cursor_composer`]).
//!
//! Each test builds a small synthetic `state.vscdb` (and, for the DAG test, a
//! `store.db`) with rusqlite, then drives the read-only composer sweep and
//! asserts the mapped rows, JSONL dedupe, incremental watermark, and malformed
//! tolerance. No real Cursor data is touched.

use std::collections::HashSet;
use std::path::Path;

use tempfile::TempDir;
use tracedecay::application::host_admission::HostAdmissionScope;
use tracedecay::sessions::SessionProvider;
use tracedecay::sessions::cursor::{CursorSweepSource, cursor_project_slug};
use tracedecay::sessions::cursor_composer::CursorComposerSource;
use tracedecay_store::ObservationReplayRequest;

use crate::common::{EnvVarGuard, GLOBAL_DB_ENV_LOCK};
use crate::restart_atomicity::{
    ProjectSessionTestRuntime, durable_table_count, ingest_global_sources_for_provider,
    mark_test_project, observation_source_cursor, open_project_session_db, set_projection_failure,
    try_ingest_source,
};
use crate::support::{init_git_repo, init_project};

const CAP: usize = 256;

async fn composer_workflow_fact_count(runtime: &ProjectSessionTestRuntime) -> u64 {
    runtime
        .runtime()
        .project_observation_table_count_for_test("observation_workflow_facts")
        .await
        .unwrap()
}

async fn composer_observation_json_blobs(runtime: &ProjectSessionTestRuntime) -> Vec<String> {
    runtime
        .runtime()
        .replay_observations(
            HostAdmissionScope::Project,
            ObservationReplayRequest::new(0, 1_000).unwrap(),
        )
        .await
        .unwrap()
        .into_iter()
        .map(|row| serde_json::to_string(row.observation()).unwrap())
        .collect()
}

/// Extracts `(input_tokens, output_tokens)` from the first uncorrelated-usage
/// fact inside one durable-observation JSON blob.
fn uncorrelated_usage_counters(observation_json: &str) -> (Option<u64>, Option<u64>) {
    let observation: serde_json::Value = serde_json::from_str(observation_json).unwrap();
    let fact = observation["payload"]["facts"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|fact| fact["kind"] == "uncorrelated_usage")
        .expect("observation blob carries an uncorrelated_usage fact");
    (
        fact["input_tokens"].as_u64(),
        fact["output_tokens"].as_u64(),
    )
}

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
    let conn = rusqlite::Connection::open(&path).unwrap();
    // DELETE journalling keeps everything in the main file so the immutable
    // read path sees it without a -wal sidecar.
    conn.execute_batch(
        "PRAGMA journal_mode=DELETE;\n\
         CREATE TABLE IF NOT EXISTS cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);",
    )
    .unwrap();
    for (key, value) in rows {
        conn.execute(
            "INSERT OR REPLACE INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
            rusqlite::params![key, value],
        )
        .unwrap();
    }
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
        .ingest(
            &db.runtime().facade(),
            &project,
            db.project_id().clone(),
            CAP,
        )
        .await
        .expect("composer sweep");
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
    // Token accounting moved to the observation family. A composer bubble's
    // tokenCount has no native model/scope/semantics evidence, so it survives
    // as an uncorrelated-usage fact on the durable observation — never as
    // message metadata, and never as a correlated provider-usage row.
    assert!(meta.get("usage").is_none());
    let uncorrelated = composer_observation_json_blobs(&db)
        .await
        .into_iter()
        .find(|blob| blob.contains("uncorrelated_usage"))
        .expect("bubble tokenCount must survive as uncorrelated usage evidence");
    let (input_tokens, output_tokens) = uncorrelated_usage_counters(&uncorrelated);
    assert_eq!((input_tokens, output_tokens), (Some(1200), Some(340)));
    assert!(
        db.provider_usage_observations("cursor").await.is_empty(),
        "uncorrelated usage evidence must not fabricate a correlated row"
    );

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

    // Envelope todos admit as WorkflowLifecycle TodoList/TodoItem (searchable).
    let first = db
        .search_session_messages("cursor", None, "First todo", 10)
        .await;
    assert!(
        first.iter().any(|hit| {
            hit.message.text.contains("First todo")
                && hit.message.metadata_json.as_deref().is_some_and(|meta| {
                    meta.contains("\"item_id\":\"t1\"") && meta.contains("completed")
                })
        }),
        "envelope todo t1 must project with native id/status"
    );
    let second = db
        .search_session_messages("cursor", None, "Second todo", 10)
        .await;
    assert!(
        second.iter().any(|hit| {
            hit.message.text.contains("Second todo")
                && hit
                    .message
                    .metadata_json
                    .as_deref()
                    .is_some_and(|meta| meta.contains("pending"))
        }),
        "envelope todo t2 must project with native pending status"
    );
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
        .ingest(
            &db.runtime().facade(),
            &project,
            db.project_id().clone(),
            CAP,
        )
        .await
        .expect("composer sweep");
    assert!(outcome.owned_session_ids.contains("comp-1"));

    // JSONL sweep with the composer-owned skip set does not touch comp-1.
    let owned: HashSet<String> = outcome.owned_session_ids.clone();
    let skipped_sweep = CursorSweepSource::with_home(&home).with_skip_session_ids(owned);
    let skipped = try_ingest_source(&db, &skipped_sweep, &project, None)
        .await
        .unwrap();
    assert_eq!(
        skipped.messages_upserted, 0,
        "owned session must be skipped"
    );

    // Control: without the skip set the same JSONL file would ingest.
    let _ = CursorComposerSource::with_home(&home)
        .ingest(
            &db.runtime().facade(),
            &project,
            db.project_id().clone(),
            CAP,
        )
        .await
        .expect("composer sweep");
    let plain_sweep = CursorSweepSource::with_home(&home);
    let plain = try_ingest_source(&db, &plain_sweep, &project, None)
        .await
        .unwrap();
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
    let first = source
        .ingest(
            &db.runtime().facade(),
            &project,
            db.project_id().clone(),
            CAP,
        )
        .await
        .expect("composer sweep");
    assert_eq!(first.sessions_upserted, 1);

    let second = source
        .ingest(
            &db.runtime().facade(),
            &project,
            db.project_id().clone(),
            CAP,
        )
        .await
        .expect("composer sweep");
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
    let third = source
        .ingest(
            &db.runtime().facade(),
            &project,
            db.project_id().clone(),
            CAP,
        )
        .await
        .expect("composer sweep");
    assert_eq!(third.sessions_upserted, 1, "growth re-ingests");
    let reply = db
        .get_session_message("cursor", "comp-1:b2")
        .await
        .expect("new bubble row");
    assert!(reply.text.contains("Second turn reply"));
}

#[tokio::test]
// This test mutates process-wide HOME while asynchronous storage work runs;
// it must hold the shared environment lock for the full test.
#[allow(clippy::await_holding_lock)]
async fn composer_projection_failure_commits_frontier_and_replays_once() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let home = tmp.path().join("home");
    let _home = EnvVarGuard::set("HOME", &home);
    init_git_repo(&project);
    mark_test_project(&project);

    let env = envelope("comp-crash", &project, &["b1"]);
    let b1 = serde_json::json!({ "type": 1, "text": "Composer crash prefix." });
    write_state_vscdb(
        &home,
        &[
            kv("composerData:comp-crash", &env),
            kv("bubbleId:comp-crash:b1", &b1),
        ],
    )
    .await;
    let db = open_project_session_db(&project).await.unwrap();
    let first =
        ingest_global_sources_for_provider(&db, &project, Some(SessionProvider::Cursor)).await;
    assert!(first.messages_upserted >= 1);
    assert!(db.get_session("cursor", "comp-crash").await.is_some());
    let prefix_cursor = observation_source_cursor(&db, "cursor", "comp-crash", &project)
        .await
        .expect("committed Cursor composer observation cursor");
    assert!(prefix_cursor.position() >= 1);
    let observations_before = durable_table_count(&db, "observations").await;
    let receipts_before = durable_table_count(&db, "sanitization_receipts").await;

    let grown = envelope("comp-crash", &project, &["b1", "b2"]);
    let b2 = serde_json::json!({ "type": 2, "text": "Composer projection retry suffix." });
    write_state_vscdb(
        &home,
        &[
            kv("composerData:comp-crash", &grown),
            kv("bubbleId:comp-crash:b1", &b1),
            kv("bubbleId:comp-crash:b2", &b2),
        ],
    )
    .await;

    set_projection_failure(&db, true).await;
    let _ = ingest_global_sources_for_provider(&db, &project, Some(SessionProvider::Cursor)).await;
    let committed_cursor = observation_source_cursor(&db, "cursor", "comp-crash", &project)
        .await
        .expect("committed Cursor composer observation cursor");
    assert_eq!(committed_cursor.generation(), prefix_cursor.generation());
    assert!(committed_cursor.position() > prefix_cursor.position());
    // Canonical observation + receipt commit before V1 projection acknowledgement.
    let committed_observations = durable_table_count(&db, "observations").await;
    let committed_receipts = durable_table_count(&db, "sanitization_receipts").await;
    assert!(committed_observations > observations_before);
    assert!(committed_receipts > receipts_before);
    assert!(durable_table_count(&db, "projection_queue").await >= 1);
    assert!(
        db.search_session_messages("cursor", None, "projection retry suffix", 10)
            .await
            .is_empty()
    );
    set_projection_failure(&db, false).await;
    drop(db);

    let recovered = open_project_session_db(&project).await.unwrap();
    let _ = ingest_global_sources_for_provider(&recovered, &project, Some(SessionProvider::Cursor))
        .await;
    assert_eq!(
        recovered
            .search_session_messages("cursor", None, "projection retry suffix", 10)
            .await
            .len(),
        1
    );
    assert_eq!(
        observation_source_cursor(&recovered, "cursor", "comp-crash", &project).await,
        Some(committed_cursor.clone())
    );
    assert_eq!(
        durable_table_count(&recovered, "observations").await,
        committed_observations
    );
    assert_eq!(
        durable_table_count(&recovered, "sanitization_receipts").await,
        committed_receipts
    );
    assert_eq!(durable_table_count(&recovered, "projection_queue").await, 0);
    assert_eq!(
        ingest_global_sources_for_provider(&recovered, &project, Some(SessionProvider::Cursor))
            .await
            .messages_upserted,
        0
    );
    assert_eq!(
        observation_source_cursor(&recovered, "cursor", "comp-crash", &project).await,
        Some(committed_cursor)
    );
}

#[tokio::test]
async fn composer_replaced_envelope_converges_without_duplicate_bubbles() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let home = tmp.path().join("home");

    let env = envelope("comp-replaced", &project, &["b1"]);
    let b1 = serde_json::json!({ "type": 1, "text": "Original composer bubble." });
    write_state_vscdb(
        &home,
        &[
            kv("composerData:comp-replaced", &env),
            kv("bubbleId:comp-replaced:b1", &b1),
        ],
    )
    .await;
    let db = open_project_session_db(&project).await.unwrap();
    let source = CursorComposerSource::with_home(&home);
    assert_eq!(
        source
            .ingest(
                &db.runtime().facade(),
                &project,
                db.project_id().clone(),
                CAP,
            )
            .await
            .expect("composer sweep")
            .sessions_upserted,
        1
    );

    // Grow with a new bubble id (replacement/growth of the envelope), mirroring
    // the existing watermark growth contract.
    let replaced = envelope("comp-replaced", &project, &["b1", "b2"]);
    let b2 = serde_json::json!({ "type": 2, "text": "late-composer-repl-9f3a reply." });
    write_state_vscdb(
        &home,
        &[
            kv("composerData:comp-replaced", &replaced),
            kv("bubbleId:comp-replaced:b1", &b1),
            kv("bubbleId:comp-replaced:b2", &b2),
        ],
    )
    .await;
    assert_eq!(
        source
            .ingest(
                &db.runtime().facade(),
                &project,
                db.project_id().clone(),
                CAP,
            )
            .await
            .expect("composer sweep")
            .sessions_upserted,
        1
    );
    assert_eq!(
        db.search_session_messages("cursor", None, "late-composer-repl-9f3a", 10)
            .await
            .len(),
        1
    );
    assert_eq!(
        source
            .ingest(
                &db.runtime().facade(),
                &project,
                db.project_id().clone(),
                CAP,
            )
            .await
            .expect("composer sweep")
            .sessions_upserted,
        0
    );
}

/// A bubble discovered after a later header was already covered must not be
/// hidden by the positional watermark. Incremental replay must converge with a
/// clean rebuild from the final snapshot.
#[tokio::test]
async fn composer_late_header_converges_with_rebuild() {
    let incremental_tmp = TempDir::new().unwrap();
    let incremental_project = init_project(&incremental_tmp);
    let incremental_home = incremental_tmp.path().join("home");
    let b1 = serde_json::json!({ "type": 1, "text": "First prompt." });
    let b2 = serde_json::json!({ "type": 2, "text": "Late middle reply." });
    let b3 = serde_json::json!({ "type": 1, "text": "Third prompt." });

    write_state_vscdb(
        &incremental_home,
        &[
            kv(
                "composerData:comp-late",
                &envelope("comp-late", &incremental_project, &["b1", "b3"]),
            ),
            kv("bubbleId:comp-late:b1", &b1),
            kv("bubbleId:comp-late:b3", &b3),
        ],
    )
    .await;
    let incremental_db = open_project_session_db(&incremental_project).await.unwrap();
    let source = CursorComposerSource::with_home(&incremental_home);
    source
        .ingest(
            &incremental_db.runtime().facade(),
            &incremental_project,
            incremental_db.project_id().clone(),
            CAP,
        )
        .await
        .expect("composer sweep");
    write_state_vscdb(
        &incremental_home,
        &[
            kv(
                "composerData:comp-late",
                &envelope("comp-late", &incremental_project, &["b1", "b2", "b3"]),
            ),
            kv("bubbleId:comp-late:b1", &b1),
            kv("bubbleId:comp-late:b2", &b2),
            kv("bubbleId:comp-late:b3", &b3),
        ],
    )
    .await;
    source
        .ingest(
            &incremental_db.runtime().facade(),
            &incremental_project,
            incremental_db.project_id().clone(),
            CAP,
        )
        .await
        .expect("composer sweep");
    let mut incremental_messages = Vec::new();
    for bubble_id in ["b1", "b2", "b3"] {
        let message_id = format!("comp-late:{bubble_id}");
        let message = incremental_db
            .get_session_message("cursor", &message_id)
            .await
            .unwrap_or_else(|| panic!("incremental replay lost {bubble_id}"));
        incremental_messages.push((bubble_id, message.role, message.text, message.kind));
    }
    drop(incremental_db);

    let rebuild_tmp = TempDir::new().unwrap();
    let rebuild_project = init_project(&rebuild_tmp);
    let rebuild_home = rebuild_tmp.path().join("home");
    write_state_vscdb(
        &rebuild_home,
        &[
            kv(
                "composerData:comp-late",
                &envelope("comp-late", &rebuild_project, &["b1", "b2", "b3"]),
            ),
            kv("bubbleId:comp-late:b1", &b1),
            kv("bubbleId:comp-late:b2", &b2),
            kv("bubbleId:comp-late:b3", &b3),
        ],
    )
    .await;
    let rebuild_db = open_project_session_db(&rebuild_project).await.unwrap();
    CursorComposerSource::with_home(&rebuild_home)
        .ingest(
            &rebuild_db.runtime().facade(),
            &rebuild_project,
            rebuild_db.project_id().clone(),
            CAP,
        )
        .await
        .expect("composer sweep");
    for (bubble_id, role, text, kind) in incremental_messages {
        let message_id = format!("comp-late:{bubble_id}");
        let rebuilt = rebuild_db
            .get_session_message("cursor", &message_id)
            .await
            .unwrap_or_else(|| panic!("rebuild lost {bubble_id}"));
        assert_eq!(role, rebuilt.role);
        assert_eq!(text, rebuilt.text);
        assert_eq!(kind, rebuilt.kind);
    }
}

/// Reordering known headers must preserve native bubble identities while a
/// newly appended bubble remains ingestible.
#[tokio::test]
async fn composer_reordered_headers_keep_native_identity() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let home = tmp.path().join("home");
    let b1 = serde_json::json!({ "type": 1, "text": "First prompt." });
    let b2 = serde_json::json!({ "type": 2, "text": "Second reply." });
    let b3 = serde_json::json!({ "type": 1, "text": "Third prompt." });

    write_state_vscdb(
        &home,
        &[
            kv(
                "composerData:comp-reorder",
                &envelope("comp-reorder", &project, &["b1", "b2"]),
            ),
            kv("bubbleId:comp-reorder:b1", &b1),
            kv("bubbleId:comp-reorder:b2", &b2),
        ],
    )
    .await;
    let db = open_project_session_db(&project).await.unwrap();
    let source = CursorComposerSource::with_home(&home);
    source
        .ingest(
            &db.runtime().facade(),
            &project,
            db.project_id().clone(),
            CAP,
        )
        .await
        .expect("composer sweep");
    write_state_vscdb(
        &home,
        &[
            kv(
                "composerData:comp-reorder",
                &envelope("comp-reorder", &project, &["b2", "b1", "b3"]),
            ),
            kv("bubbleId:comp-reorder:b1", &b1),
            kv("bubbleId:comp-reorder:b2", &b2),
            kv("bubbleId:comp-reorder:b3", &b3),
        ],
    )
    .await;
    source
        .ingest(
            &db.runtime().facade(),
            &project,
            db.project_id().clone(),
            CAP,
        )
        .await
        .expect("composer sweep");
    for (bubble_id, text) in [
        ("b1", "First prompt."),
        ("b2", "Second reply."),
        ("b3", "Third prompt."),
    ] {
        let message = db
            .get_session_message("cursor", &format!("comp-reorder:{bubble_id}"))
            .await
            .unwrap_or_else(|| panic!("reordered replay lost {bubble_id}"));
        assert_eq!(message.text, text);
    }
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
        .ingest(
            &db.runtime().facade(),
            &project,
            db.project_id().clone(),
            CAP,
        )
        .await
        .expect("composer sweep");
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
    let expected_store_path = agent_dir.join("store.db");
    assert_eq!(
        store_session.transcript_path.as_deref(),
        Some(expected_store_path.to_string_lossy().as_ref())
    );
}

/// Oversized bubble TEXT built in SQL must not become a durable message row.
#[tokio::test]
async fn composer_sql_oversized_bubble_is_non_durable_without_payload_leak() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let home = tmp.path().join("home");

    let env = envelope("comp-oversize", &project, &["b-ok", "b-huge"]);
    let ok_bubble = serde_json::json!({ "type": 1, "text": "small ok bubble" });
    // Build the hostile bubble value entirely inside SQLite.
    let dir = home
        .join(".config")
        .join("Cursor")
        .join("User")
        .join("globalStorage");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("state.vscdb");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode=DELETE;\n\
         CREATE TABLE IF NOT EXISTS cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);",
    )
    .unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
        rusqlite::params!["composerData:comp-oversize", env.to_string()],
    )
    .unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
        rusqlite::params!["bubbleId:comp-oversize:b-ok", ok_bubble.to_string()],
    )
    .unwrap();
    // 1 MiB + 2 of hex zeros via zeroblob — never a Rust String of that size
    // in the product path. length(value) = 2 * 524289 = 1_048_578.
    conn.execute(
        "INSERT OR REPLACE INTO cursorDiskKV (key, value) \
         SELECT 'bubbleId:comp-oversize:b-huge', hex(zeroblob(524289))",
        (),
    )
    .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let outcome = CursorComposerSource::with_home(&home)
        .ingest_capped(
            &db.runtime().facade(),
            &project,
            db.project_id().clone(),
            CAP,
            Some(64 * 1024),
        )
        .await
        .expect("composer sweep");
    assert!(
        outcome.owned_session_ids.contains("comp-oversize"),
        "valid envelope still owns the session"
    );
    assert!(
        db.get_session_message("cursor", "comp-oversize:b-huge")
            .await
            .is_none(),
        "oversized bubble must not persist"
    );
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
        root.extend_from_slice(&hex::decode(id).unwrap());
    }

    let meta = serde_json::json!({
        "agentId": "agent-1",
        "latestRootBlobId": root_id,
        "name": "Store chat",
        "mode": "agent",
        "createdAt": 1_700_000_100_000i64,
    });
    let meta_hex = hex::encode(meta.to_string().as_bytes());

    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode=DELETE;\n\
         CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);\n\
         CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO meta (key, value) VALUES ('0', ?1)",
        rusqlite::params![meta_hex],
    )
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
            rusqlite::params![id, data],
        )
        .unwrap();
    }
}

/// Production path: checked-in envelope todos admit as WorkflowLifecycle facts
/// with stable list/item refs and native array order.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn composer_envelope_todos_admit_workflow_lifecycle_facts() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let home = tmp.path().join("home");
    let _home = EnvVarGuard::set("HOME", &home);
    init_git_repo(&project);
    mark_test_project(&project);

    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../fixtures/provider_normalization/cursor_composer/envelope_todos.input.json"
    ))
    .expect("checked-in composer envelope todos fixture");
    let mut env = envelope("comp-1", &project, &["b-user"]);
    env["todos"] = fixture["todos"].clone();
    let user_bubble = serde_json::json!({
        "type": 1,
        "text": "Please work the checklist."
    });

    write_state_vscdb(
        &home,
        &[
            kv("composerData:comp-1", &env),
            kv("bubbleId:comp-1:b-user", &user_bubble),
        ],
    )
    .await;
    let db = open_project_session_db(&project).await.unwrap();
    let _ = ingest_global_sources_for_provider(&db, &project, Some(SessionProvider::Cursor)).await;
    assert_eq!(
        durable_table_count(&db, "projection_queue").await,
        0,
        "envelope lifecycle projection must be applied synchronously"
    );
    assert_eq!(
        composer_workflow_fact_count(&db).await,
        3,
        "one TodoList and two TodoItem facts must be projected"
    );

    let hits = db
        .search_session_messages("cursor", None, "First todo", 10)
        .await;
    assert_eq!(hits.len(), 1, "todo item content must be searchable");
    assert_eq!(hits[0].message.kind.as_deref(), Some("todo_item"));
    let meta: serde_json::Value =
        serde_json::from_str(hits[0].message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(meta["item_id"], "t1");
    assert_eq!(meta["list_reference"], "comp-1");
    assert_eq!(meta["status"], "completed");
    assert_eq!(meta["item_order"], 0);
    assert!(meta.get("revision").is_none());
    assert_eq!(meta["provider_reference"], "t1");

    let second = db
        .search_session_messages("cursor", None, "Second todo", 10)
        .await;
    assert_eq!(second.len(), 1);
    let second_meta: serde_json::Value =
        serde_json::from_str(second[0].message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(second_meta["item_id"], "t2");
    assert_eq!(second_meta["status"], "pending");
    assert_eq!(second_meta["item_order"], 1);
    assert_eq!(second_meta["list_reference"], "comp-1");

    // Co-located bubble Message remains searchable alongside WorkflowLifecycle.
    assert_eq!(
        db.search_session_messages("cursor", None, "Please work the checklist", 10)
            .await
            .len(),
        1
    );
}

/// Exact redelivery of the same envelope todos is idempotent.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn composer_envelope_todos_exact_duplicate_is_idempotent() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let home = tmp.path().join("home");
    let _home = EnvVarGuard::set("HOME", &home);
    init_git_repo(&project);
    mark_test_project(&project);

    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../fixtures/provider_normalization/cursor_composer/envelope_todos.input.json"
    ))
    .unwrap();
    let mut env = envelope("comp-1", &project, &["b-user"]);
    env["todos"] = fixture["todos"].clone();
    let user_bubble = serde_json::json!({ "type": 1, "text": "Idempotent checklist prompt." });
    write_state_vscdb(
        &home,
        &[
            kv("composerData:comp-1", &env),
            kv("bubbleId:comp-1:b-user", &user_bubble),
        ],
    )
    .await;
    let db = open_project_session_db(&project).await.unwrap();
    let _ = ingest_global_sources_for_provider(&db, &project, Some(SessionProvider::Cursor)).await;
    let observations_before = durable_table_count(&db, "observations").await;
    let workflow_before = composer_workflow_fact_count(&db).await;
    let _ = ingest_global_sources_for_provider(&db, &project, Some(SessionProvider::Cursor)).await;
    assert_eq!(
        durable_table_count(&db, "observations").await,
        observations_before,
        "unchanged envelope todos must not create observations"
    );
    assert_eq!(composer_workflow_fact_count(&db).await, workflow_before);
    assert_eq!(
        db.search_session_messages("cursor", None, "First todo", 10)
            .await
            .len(),
        1
    );
}

#[tokio::test]
async fn composer_envelope_todo_secret_is_sanitized_before_persistence() {
    const SECRET: &str = "AKIACOMPOSERTODO0001";
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let home = tmp.path().join("home");
    let mut env = envelope("comp-secret", &project, &[]);
    env["todos"] = serde_json::json!([
        {
            "id": "todo-secret",
            "content": format!("rotate access key {SECRET}"),
            "status": "pending"
        }
    ]);
    write_state_vscdb(&home, &[kv("composerData:comp-secret", &env)]).await;
    let db = open_project_session_db(&project).await.unwrap();
    CursorComposerSource::with_home(&home)
        .ingest(
            &db.runtime().facade(),
            &project,
            db.project_id().clone(),
            CAP,
        )
        .await
        .expect("composer sweep");
    let joined = composer_observation_json_blobs(&db).await.join("\n");
    assert!(joined.contains("workflow_lifecycle"));
    assert!(
        !joined.contains(SECRET),
        "secret-bearing todo content must be sanitized before persistence"
    );
}

/// Same todo checkpoint with divergent envelope evidence (createdAt) after a
/// generation change is an identity collision — first durable facts remain.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn composer_envelope_todos_conflict_does_not_overwrite() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let home = tmp.path().join("home");
    let _home = EnvVarGuard::set("HOME", &home);
    init_git_repo(&project);
    mark_test_project(&project);

    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../fixtures/provider_normalization/cursor_composer/envelope_todos.input.json"
    ))
    .unwrap();
    let mut env = envelope("comp-1", &project, &["b-user"]);
    env["todos"] = fixture["todos"].clone();
    let user_bubble = serde_json::json!({ "type": 1, "text": "Conflict checklist prompt." });
    write_state_vscdb(
        &home,
        &[
            kv("composerData:comp-1", &env),
            kv("bubbleId:comp-1:b-user", &user_bubble),
        ],
    )
    .await;

    let db = open_project_session_db(&project).await.unwrap();
    let _ = ingest_global_sources_for_provider(&db, &project, Some(SessionProvider::Cursor)).await;
    let workflow_before = composer_workflow_fact_count(&db).await;
    assert_eq!(workflow_before, 3);

    // New snapshot generation, same todos (same content fingerprint checkpoint),
    // but divergent createdAt → same native identity, different payload digest.
    let state_db = home
        .join(".config")
        .join("Cursor")
        .join("User")
        .join("globalStorage")
        .join("state.vscdb");
    std::fs::remove_file(&state_db).unwrap();
    env["createdAt"] = serde_json::json!(1_700_000_000_999_i64);
    write_state_vscdb(
        &home,
        &[
            kv("composerData:comp-1", &env),
            kv("bubbleId:comp-1:b-user", &user_bubble),
        ],
    )
    .await;

    let _ = ingest_global_sources_for_provider(&db, &project, Some(SessionProvider::Cursor)).await;

    assert_eq!(
        composer_workflow_fact_count(&db).await,
        workflow_before,
        "identity collision must not project a second todo snapshot"
    );
    assert_eq!(
        db.search_session_messages("cursor", None, "First todo", 10)
            .await
            .len(),
        1,
        "original envelope todo content must remain"
    );
}

/// Fixture-backed pending→completed status update after restart admits a new
/// content-fingerprint checkpoint without inventing revision fields.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn composer_envelope_todo_status_update_after_restart_admits_new_checkpoint() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let home = tmp.path().join("home");
    let _home = EnvVarGuard::set("HOME", &home);
    init_git_repo(&project);
    mark_test_project(&project);

    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../fixtures/provider_normalization/cursor_composer/envelope_todos.input.json"
    ))
    .unwrap();
    assert!(fixture.get("lastUpdatedAt").is_some_and(|v| v.is_null()));
    let mut env = envelope("comp-1", &project, &["b-user"]);
    env["todos"] = fixture["todos"].clone();
    let user_bubble = serde_json::json!({ "type": 1, "text": "Update checklist prompt." });
    write_state_vscdb(
        &home,
        &[
            kv("composerData:comp-1", &env),
            kv("bubbleId:comp-1:b-user", &user_bubble),
        ],
    )
    .await;

    let db = open_project_session_db(&project).await.unwrap();
    let _ = ingest_global_sources_for_provider(&db, &project, Some(SessionProvider::Cursor)).await;
    let pending = db
        .search_session_messages("cursor", None, "Second todo", 10)
        .await;
    assert_eq!(pending.len(), 1);
    let pending_meta: serde_json::Value =
        serde_json::from_str(pending[0].message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(pending_meta["status"], "pending");
    assert_eq!(pending_meta["item_order"], 1);
    assert!(pending_meta.get("revision").is_none());
    drop(db);

    // Restart against the same vscdb inode, then apply native status, content,
    // and array-order changes without inventing a revision.
    env["todos"][1]["status"] = serde_json::json!("completed");
    env["todos"][1]["content"] = serde_json::json!("Second todo revised");
    env["todos"].as_array_mut().unwrap().swap(0, 1);
    write_state_vscdb(
        &home,
        &[
            kv("composerData:comp-1", &env),
            kv("bubbleId:comp-1:b-user", &user_bubble),
        ],
    )
    .await;
    let db = open_project_session_db(&project).await.unwrap();
    let _ = ingest_global_sources_for_provider(&db, &project, Some(SessionProvider::Cursor)).await;
    assert_eq!(
        composer_workflow_fact_count(&db).await,
        6,
        "restart update must project a second list snapshot"
    );

    let hits = db
        .search_session_messages("cursor", None, "Second todo", 10)
        .await;
    assert!(
        hits.len() >= 2,
        "status update must admit a new checkpointed observation; got {}",
        hits.len()
    );
    let statuses: Vec<String> = hits
        .iter()
        .filter_map(|hit| {
            let meta: serde_json::Value =
                serde_json::from_str(hit.message.metadata_json.as_deref().unwrap()).unwrap();
            assert_eq!(meta["item_id"], "t2");
            assert_eq!(meta["list_reference"], "comp-1");
            assert!(meta.get("revision").is_none());
            meta["status"].as_str().map(str::to_string)
        })
        .collect();
    assert!(statuses.iter().any(|s| s == "pending"));
    assert!(statuses.iter().any(|s| s == "completed"));
    let revised = hits
        .iter()
        .find(|hit| hit.message.text.contains("Second todo revised"))
        .expect("updated todo content must be searchable after restart");
    let revised_meta: serde_json::Value =
        serde_json::from_str(revised.message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(revised_meta["status"], "completed");
    assert_eq!(revised_meta["item_order"], 0);
    assert!(revised_meta.get("revision").is_none());
    let first_todo = db
        .search_session_messages("cursor", None, "First todo", 10)
        .await;
    assert_eq!(
        first_todo.len(),
        2,
        "the sibling's native order transition must remain visible"
    );
    let mut first_orders = first_todo
        .iter()
        .map(|hit| {
            serde_json::from_str::<serde_json::Value>(hit.message.metadata_json.as_deref().unwrap())
                .unwrap()["item_order"]
                .as_u64()
                .unwrap()
        })
        .collect::<Vec<_>>();
    first_orders.sort_unstable();
    assert_eq!(first_orders, vec![0, 1]);
}

/// A bounded git timeout (`Unknown` membership) must skip the envelope
/// without advancing its watermark, so a later sweep can re-resolve it.
#[cfg(unix)]
#[tokio::test]
async fn composer_unknown_project_membership_defers_persistence_and_watermark() {
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
            .ingest(
                &db.runtime().facade(),
                &project,
                db.project_id().clone(),
                CAP,
            )
            .await
            .expect("composer sweep");
        assert_eq!(outcome.sessions_upserted, 0);
        assert!(outcome.owned_session_ids.is_empty());
        assert!(db.get_session("cursor", "unknown-composer").await.is_none());
        assert!(
            observation_source_cursor(&db, "cursor", "unknown-composer", &project)
                .await
                .is_none()
        );
        return;
    }

    crate::vibe::run_unknown_membership_child(
        CHILD_ENV,
        "cursor_composer::composer_unknown_project_membership_defers_persistence_and_watermark",
    );
}
