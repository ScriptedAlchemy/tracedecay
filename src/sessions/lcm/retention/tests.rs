use std::path::PathBuf;

use crate::db::engine::{Connection, Executor, IntoParams, QueryExecutor, TestConnection, params};
use crate::sessions::lcm::schema;

use super::*;

const PROVIDER: &str = "cursor";
const SESSION: &str = "session-a";
const DAY: i64 = 24 * 60 * 60;
const NOW: i64 = 1_900_000_000;

struct TestStore {
    conn: Connection,
    _runtime: TestConnection,
    storage_root: PathBuf,
    _temp: tempfile::TempDir,
}

async fn test_store() -> Result<TestStore, String> {
    let temp = tempfile::tempdir().map_err(|err| format!("tempdir: {err}"))?;
    let storage_root = temp.path().to_path_buf();
    let runtime = TestConnection::open(&storage_root.join("sessions.db"));
    let conn = (*runtime).clone();
    conn.execute_batch(
        "CREATE TABLE sessions (
            provider TEXT NOT NULL,
            session_id TEXT NOT NULL,
            project_key TEXT NOT NULL,
            project_path TEXT NOT NULL,
            title TEXT,
            started_at INTEGER,
            PRIMARY KEY(provider, session_id)
        );
        CREATE TABLE session_messages (
            provider TEXT NOT NULL,
            message_id TEXT NOT NULL,
            session_id TEXT NOT NULL,
            role TEXT NOT NULL,
            timestamp INTEGER,
            ordinal INTEGER NOT NULL,
            text TEXT NOT NULL,
            kind TEXT,
            model TEXT,
            tool_names TEXT,
            source_path TEXT,
            source_offset INTEGER,
            metadata_json TEXT,
            PRIMARY KEY(provider, message_id),
            FOREIGN KEY(provider, session_id)
                REFERENCES sessions(provider, session_id) ON DELETE CASCADE
        );
        CREATE VIRTUAL TABLE session_messages_fts USING fts5(
            text, role, kind, model, tool_names,
            content='session_messages', content_rowid='rowid'
        );
        CREATE TRIGGER session_messages_fts_insert
            AFTER INSERT ON session_messages BEGIN
                INSERT INTO session_messages_fts(rowid, text, role, kind, model, tool_names)
                VALUES (NEW.rowid, NEW.text, NEW.role, NEW.kind, NEW.model, NEW.tool_names);
            END;
        CREATE TRIGGER session_messages_fts_delete
            AFTER DELETE ON session_messages BEGIN
                INSERT INTO session_messages_fts(session_messages_fts, rowid, text, role, kind, model, tool_names)
                VALUES ('delete', OLD.rowid, OLD.text, OLD.role, OLD.kind, OLD.model, OLD.tool_names);
            END;",
    )
    .await
    .map_err(|err| format!("seed sessions schema: {err}"))?;
    schema::ensure_lcm_schema(&conn)
        .await
        .map_err(|err| format!("ensure lcm schema: {err}"))?;
    conn.execute(
        "INSERT INTO sessions(provider, session_id, project_key, project_path)
         VALUES (?1, ?2, '/p', '/p')",
        params![PROVIDER, SESSION],
    )
    .await
    .map_err(|err| format!("insert session: {err}"))?;
    Ok(TestStore {
        conn,
        _runtime: runtime,
        storage_root,
        _temp: temp,
    })
}

/// Inserts an inline raw message (and its projected `session_messages` twin)
/// with the given age. Returns the assigned `store_id`.
async fn insert_message(
    conn: &(impl Executor + ?Sized),
    ordinal: i64,
    age_days: i64,
    content: &str,
) -> Result<i64, String> {
    let message_id = format!("msg-{ordinal}");
    let timestamp = NOW - age_days * DAY;
    let hash = crate::sessions::lcm::util::sha256_hex(content.as_bytes());
    conn.execute(
        "INSERT INTO lcm_raw_messages (
            provider, message_id, session_id, role, ordinal, timestamp,
            content, content_hash, storage_kind, payload_ref, snippet_text,
            index_text, metadata_json
         )
         VALUES (?1, ?2, ?3, 'assistant', ?4, ?5, ?6, ?7, 'inline', NULL, ?6, ?6, NULL)",
        params![
            PROVIDER,
            message_id.as_str(),
            SESSION,
            ordinal,
            timestamp,
            content,
            hash.as_str()
        ],
    )
    .await
    .map_err(|err| format!("insert raw: {err}"))?;
    conn.execute(
        "INSERT INTO session_messages(provider, message_id, session_id, role, timestamp, ordinal, text)
         VALUES (?1, ?2, ?3, 'assistant', ?4, ?5, ?6)",
        params![
            PROVIDER,
            message_id.as_str(),
            SESSION,
            timestamp,
            ordinal,
            content
        ],
    )
    .await
    .map_err(|err| format!("insert projected: {err}"))?;
    let store_id = fetch_i64(
        conn,
        "SELECT store_id FROM lcm_raw_messages WHERE provider = ?1 AND message_id = ?2",
        params![PROVIDER, message_id.as_str()],
    )
    .await?;
    Ok(store_id)
}

/// Marks a raw row projection-durable by adding a summary node whose lineage
/// covers `store_id`.
async fn make_projection_durable(
    conn: &(impl Executor + ?Sized),
    store_id: i64,
) -> Result<(), String> {
    let node_id = format!("node-{store_id}");
    conn.execute(
        "INSERT INTO lcm_summary_nodes(
            node_id, provider, conversation_id, session_id, depth, summary_text,
            summary_hash, summary_token_count, source_token_count
         )
         VALUES (?1, ?2, 'conv', ?3, 0, 'summary', 'h', 1, 1)",
        params![node_id.as_str(), PROVIDER, SESSION],
    )
    .await
    .map_err(|err| format!("insert summary node: {err}"))?;
    conn.execute(
        "INSERT INTO lcm_summary_sources(node_id, source_kind, source_id, ordinal)
         VALUES (?1, 'raw_message', ?2, 0)",
        params![node_id.as_str(), store_id.to_string()],
    )
    .await
    .map_err(|err| format!("insert summary source: {err}"))?;
    Ok(())
}

async fn fetch_i64(
    conn: &(impl QueryExecutor + ?Sized),
    sql: &str,
    params: impl IntoParams,
) -> Result<i64, String> {
    let mut rows = conn.query(sql, params).await.map_err(|e| e.to_string())?;
    let row = rows
        .next()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no row".to_string())?;
    row.get::<i64>(0).map_err(|e| e.to_string())
}

async fn count(conn: &(impl QueryExecutor + ?Sized), table: &str) -> Result<i64, String> {
    fetch_i64(conn, &format!("SELECT COUNT(*) FROM {table}"), ()).await
}

fn drop_config(days: u32) -> LcmRetentionConfig {
    LcmRetentionConfig {
        enabled: true,
        drop_after_days: Some(days),
        dedupe_projected_after_days: None,
        ..LcmRetentionConfig::default()
    }
}

async fn run_apply(
    conn: &Connection,
    storage_root: &std::path::Path,
    config: &LcmRetentionConfig,
) -> Result<LcmRetentionReport, String> {
    run_session_retention_authorized(
        conn,
        storage_root,
        PROVIDER,
        None,
        config,
        RetentionMode::Apply,
        NOW,
        &|_| Ok(()),
    )
    .await
    .map_err(|error| error.to_string())
}

#[tokio::test]
async fn authority_loss_before_commit_rolls_back_retention_mutations() -> Result<(), String> {
    let store = test_store().await?;
    let durable = insert_message(&store.conn, 1, 90, "must survive").await?;
    make_projection_durable(&store.conn, durable).await?;
    let scope = std::sync::Mutex::new(Some(
        crate::db::enter_daemon_database_scope(
            &store.storage_root,
            1,
            "session-retention-revocation-test",
        )
        .map_err(|error| error.to_string())?,
    ));
    let authority = crate::db::DatabaseAuthority::for_runtime(
        &store.storage_root.join("sessions.db"),
        "session retention revocation test",
    )
    .map_err(|error| error.to_string())?;
    let commit_revoked = std::sync::atomic::AtomicBool::new(false);

    let error = run_session_retention_authorized(
        &store.conn,
        &store.storage_root,
        PROVIDER,
        None,
        &drop_config(30),
        RetentionMode::Apply,
        NOW,
        &|intent| {
            if intent == "commit session retention drop pass" {
                commit_revoked.store(true, std::sync::atomic::Ordering::SeqCst);
                drop(
                    scope
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take(),
                );
            }
            authority
                .require_active_write_scope(intent)
                .map_err(|error| LcmError::Db(error.to_string()))
        },
    )
    .await
    .expect_err("authority loss must reject retention commit");

    assert!(error.to_string().contains("active daemon"));
    assert!(
        commit_revoked.load(std::sync::atomic::Ordering::SeqCst),
        "authority is revoked only after the drop mutations, at precommit"
    );
    assert_eq!(count(&store.conn, "lcm_raw_messages").await?, 1);
    assert_eq!(count(&store.conn, "session_messages").await?, 1);
    Ok(())
}

// (a)+(d) drop acts only on projection-durable rows; un-projected live evidence
// is never deleted, even when older than the window.
#[tokio::test]
async fn drop_reaps_only_projection_durable_rows() -> Result<(), String> {
    let store = test_store().await?;
    let conn = &store.conn;
    let durable = insert_message(conn, 1, 90, "durable old content").await?;
    let _live = insert_message(conn, 2, 90, "live un-projected content").await?;
    make_projection_durable(conn, durable).await?;

    let report = run_apply(conn, &store.storage_root, &drop_config(30)).await?;

    assert_eq!(
        report.dropped.eligible, 1,
        "only the durable row is eligible"
    );
    assert_eq!(report.dropped.acted, 1);
    assert_eq!(
        count(conn, "lcm_raw_messages").await?,
        1,
        "live row retained"
    );
    // The surviving raw row is the un-projected live one.
    let survivor: i64 = fetch_i64(conn, "SELECT store_id FROM lcm_raw_messages", ()).await?;
    assert_ne!(survivor, durable, "durable row dropped, live row kept");
    // Projected twin of the dropped row is gone; the live twin remains.
    assert_eq!(count(conn, "session_messages").await?, 1);
    assert!(report.dropped.bytes_reclaimed > 0, "reclaim is measurable");
    Ok(())
}

// (b) retention window is honored: a projection-durable row inside the window
// is not dropped.
#[tokio::test]
async fn drop_honors_retention_window() -> Result<(), String> {
    let store = test_store().await?;
    let conn = &store.conn;
    let recent = insert_message(conn, 1, 10, "recent durable").await?;
    let old = insert_message(conn, 2, 90, "old durable").await?;
    make_projection_durable(conn, recent).await?;
    make_projection_durable(conn, old).await?;

    let report = run_apply(conn, &store.storage_root, &drop_config(30)).await?;

    assert_eq!(report.dropped.acted, 1, "only the >30d row is dropped");
    let survivor: i64 = fetch_i64(conn, "SELECT store_id FROM lcm_raw_messages", ()).await?;
    assert_eq!(survivor, recent, "row inside the window is retained");
    Ok(())
}

// (b) dry run counts eligible reclaim without mutating anything.
#[tokio::test]
async fn dry_run_counts_without_mutating() -> Result<(), String> {
    let store = test_store().await?;
    let conn = &store.conn;
    let durable = insert_message(conn, 1, 90, "durable old content").await?;
    make_projection_durable(conn, durable).await?;

    let report = run_session_retention_authorized(
        conn,
        &store.storage_root,
        PROVIDER,
        None,
        &drop_config(30),
        RetentionMode::DryRun,
        NOW,
        &|_| Ok(()),
    )
    .await
    .map_err(|e| e.to_string())?;

    assert_eq!(report.dropped.eligible, 1);
    assert_eq!(report.dropped.acted, 0, "dry run acts on nothing");
    assert_eq!(
        report.dropped.oldest_eligible_at,
        Some(NOW - 90 * DAY),
        "backlog age comes from the oldest real eligible row"
    );
    assert!(report.dropped.bytes_reclaimed > 0, "dry run still measures");
    assert_eq!(count(conn, "lcm_raw_messages").await?, 1, "no mutation");
    Ok(())
}

#[tokio::test]
async fn backlog_read_reports_real_eligible_bytes_and_watermark() -> Result<(), String> {
    let store = test_store().await?;
    let durable = insert_message(&store.conn, 1, 90, "retention backlog bytes").await?;
    make_projection_durable(&store.conn, durable).await?;

    let records = read_session_retention_backlog(
        &store.conn,
        tracedecay_application::storage::StoreKeyV1::new("sessions.db")
            .map_err(|error| error.to_string())?,
        &drop_config(30),
        NOW,
    )
    .await
    .map_err(|error| error.to_string())?;

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].table.as_str(), "lcm_raw_messages");
    assert!(records[0].past_window_bytes.get() > 0);
    assert_eq!(
        records[0].oldest_past_window_at,
        tracedecay_domain::UtcMicros((NOW - 90 * DAY) * 1_000_000)
    );
    assert_eq!(
        records[0].window_watermark_at,
        tracedecay_domain::UtcMicros((NOW - 30 * DAY) * 1_000_000)
    );
    Ok(())
}

#[tokio::test]
async fn backlog_read_emits_clean_zero_record_for_configured_window() -> Result<(), String> {
    let store = test_store().await?;
    let records = read_session_retention_backlog(
        &store.conn,
        tracedecay_application::storage::StoreKeyV1::new("sessions.db")
            .map_err(|error| error.to_string())?,
        &drop_config(30),
        NOW,
    )
    .await
    .map_err(|error| error.to_string())?;

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].past_window_bytes.get(), 0);
    records[0].validate().map_err(|error| error.to_string())?;
    Ok(())
}

// (c)-analogue for one-content-copy: the projected twin obeys the window while
// the raw copy is retained — proving raw and projected do not both persist.
#[tokio::test]
async fn dedupe_drops_projected_duplicate_and_keeps_raw() -> Result<(), String> {
    let store = test_store().await?;
    let conn = &store.conn;
    let store_id = insert_message(conn, 1, 90, "duplicated content").await?;
    make_projection_durable(conn, store_id).await?;

    let config = LcmRetentionConfig {
        enabled: true,
        dedupe_projected_after_days: Some(30),
        ..LcmRetentionConfig::default()
    };
    let fts_before = count(conn, "session_messages_fts").await?;
    let report = run_apply(conn, &store.storage_root, &config).await?;

    assert_eq!(report.projected_deduped.acted, 1);
    assert_eq!(
        count(conn, "session_messages").await?,
        0,
        "projected twin dropped"
    );
    assert_eq!(
        count(conn, "lcm_raw_messages").await?,
        1,
        "raw copy retained"
    );
    // The projected FTS shadow obeys the same window (trigger cleaned it).
    let fts_after = count(conn, "session_messages_fts").await?;
    assert!(fts_after < fts_before, "projected FTS shadow shrank");
    Ok(())
}

#[tokio::test]
async fn dedupe_retains_projected_copy_until_summary_lineage_is_durable() -> Result<(), String> {
    let store = test_store().await?;
    let conn = &store.conn;
    insert_message(conn, 1, 90, "not durable yet").await?;

    let config = LcmRetentionConfig::default();
    let backlog = read_session_retention_backlog(
        conn,
        tracedecay_application::storage::StoreKeyV1::new("sessions.db")
            .map_err(|error| error.to_string())?,
        &config,
        NOW,
    )
    .await
    .map_err(|error| error.to_string())?;
    let projected = backlog
        .iter()
        .find(|record| record.table.as_str() == "session_messages")
        .ok_or_else(|| "missing projected retention backlog".to_string())?;
    assert_eq!(projected.past_window_bytes.get(), 0);

    let report = run_apply(conn, &store.storage_root, &config).await?;
    assert_eq!(report.projected_deduped.eligible, 0);
    assert_eq!(report.projected_deduped.acted, 0);
    assert_eq!(
        count(conn, "session_messages").await?,
        1,
        "non-durable projection remains the only immediately queryable copy"
    );
    Ok(())
}

// A projected row with NO raw twin is the sole copy and must never be deduped.
#[tokio::test]
async fn dedupe_never_touches_sole_projected_copy() -> Result<(), String> {
    let store = test_store().await?;
    let conn = &store.conn;
    // Insert a projected-only row (no raw twin), aged past the window.
    conn.execute(
        "INSERT INTO session_messages(provider, message_id, session_id, role, timestamp, ordinal, text)
         VALUES (?1, 'lonely', ?2, 'assistant', ?3, 1, 'sole copy')",
        params![PROVIDER, SESSION, NOW - 90 * DAY],
    )
    .await
    .map_err(|e| e.to_string())?;

    let config = LcmRetentionConfig {
        enabled: true,
        dedupe_projected_after_days: Some(30),
        ..LcmRetentionConfig::default()
    };
    let report = run_apply(conn, &store.storage_root, &config).await?;

    assert_eq!(report.projected_deduped.acted, 0);
    assert_eq!(
        count(conn, "session_messages").await?,
        1,
        "sole copy retained"
    );
    Ok(())
}

// (a) offload only externalizes projection-durable rows AFTER durability; the
// bulky inline content leaves the raw column, replaced by a recoverable
// content-addressed placeholder (§4 one content copy).
#[tokio::test]
async fn offload_externalizes_durable_content_after_durability() -> Result<(), String> {
    let store = test_store().await?;
    let conn = &store.conn;
    let content = "x".repeat(4096);
    let durable = insert_message(conn, 1, 90, &content).await?;
    let _live = insert_message(conn, 2, 90, &content).await?;
    make_projection_durable(conn, durable).await?;

    let config = LcmRetentionConfig {
        enabled: true,
        offload_after_days: Some(30),
        ..LcmRetentionConfig::default()
    };
    let report = run_apply(conn, &store.storage_root, &config).await?;

    assert_eq!(
        report.offloaded.acted, 1,
        "only the durable row is offloaded"
    );
    assert!(report.offloaded.bytes_reclaimed >= 4096);

    // Durable row: inline content cleared, now external with a payload_ref.
    let kind: String = fetch_str(
        conn,
        "SELECT storage_kind FROM lcm_raw_messages WHERE store_id = ?1",
        params![durable],
    )
    .await?;
    assert_eq!(kind, "external");
    let payload_present = fetch_i64(conn, "SELECT COUNT(*) FROM lcm_external_payloads", ()).await?;
    assert_eq!(payload_present, 1, "content stored once, addressed by hash");
    // The un-projected live row is untouched (still inline).
    let live_kind: i64 = fetch_i64(
        conn,
        "SELECT COUNT(*) FROM lcm_raw_messages WHERE storage_kind = 'inline'",
        (),
    )
    .await?;
    assert_eq!(live_kind, 1, "live un-projected row stays inline");
    Ok(())
}

#[tokio::test]
async fn offload_cas_preserves_revived_row_and_rolls_back_payload() -> Result<(), String> {
    let store = test_store().await?;
    let original = "stale content".repeat(128);
    let store_id = insert_message(&store.conn, 1, 90, &original).await?;
    make_projection_durable(&store.conn, store_id).await?;
    let target = OffloadRow {
        store_id,
        provider: PROVIDER.to_string(),
        session_id: SESSION.to_string(),
        message_id: "msg-1".to_string(),
        timestamp: NOW - 90 * DAY,
        content: original,
    };
    let revived = "revived content";
    let revived_hash = crate::sessions::lcm::util::sha256_hex(revived.as_bytes());
    store
        .conn
        .execute(
            "UPDATE lcm_raw_messages
             SET timestamp = ?2, content = ?3, content_hash = ?4,
                 snippet_text = ?3, index_text = ?3
             WHERE store_id = ?1",
            params![store_id, NOW, revived, revived_hash.as_str()],
        )
        .await
        .map_err(|error| error.to_string())?;

    let error = offload_one(&store.conn, &store.storage_root, &target, &|_| Ok(()))
        .await
        .expect_err("stale candidate must fail the offload compare-and-swap");

    assert!(error.to_string().contains("compare-and-swap rejected"));
    assert_eq!(
        fetch_str(
            &store.conn,
            "SELECT content FROM lcm_raw_messages WHERE store_id = ?1",
            params![store_id],
        )
        .await?,
        revived
    );
    assert_eq!(
        fetch_str(
            &store.conn,
            "SELECT storage_kind FROM lcm_raw_messages WHERE store_id = ?1",
            params![store_id],
        )
        .await?,
        "inline"
    );
    assert_eq!(count(&store.conn, "lcm_external_payloads").await?, 0);
    assert_eq!(
        std::fs::read_dir(crate::sessions::lcm::payload::payload_dir(
            &store.storage_root
        ))
        .map_err(|error| error.to_string())?
        .count(),
        0,
        "failed CAS removes the newly-created payload file"
    );
    Ok(())
}

// (e) reclaimed space is measurable via row and page/free-list metrics.
#[tokio::test]
async fn reports_measurable_reclaim_metrics() -> Result<(), String> {
    let store = test_store().await?;
    let conn = &store.conn;
    for ordinal in 1..=8 {
        let store_id = insert_message(conn, ordinal, 90, &"y".repeat(2048)).await?;
        make_projection_durable(conn, store_id).await?;
    }
    let report = run_apply(conn, &store.storage_root, &drop_config(30)).await?;

    assert_eq!(report.raw_rows_before, 8);
    assert_eq!(report.raw_rows_after, 0, "row-count delta is measurable");
    assert!(report.page_count_before > 0, "page_count observed");
    assert!(
        report.freelist_after >= report.freelist_before,
        "deleted rows freed pages"
    );
    assert!(report.bytes_reclaimed() >= 8 * 2048);
    Ok(())
}

// Disabled config is an inert no-op even in Apply mode.
#[tokio::test]
async fn disabled_config_is_a_no_op() -> Result<(), String> {
    let store = test_store().await?;
    let conn = &store.conn;
    let durable = insert_message(conn, 1, 90, "durable").await?;
    make_projection_durable(conn, durable).await?;

    let config = LcmRetentionConfig {
        enabled: false,
        drop_after_days: Some(1),
        ..LcmRetentionConfig::default()
    };
    let report = run_apply(conn, &store.storage_root, &config).await?;

    assert_eq!(report.dropped.acted, 0);
    assert_eq!(count(conn, "lcm_raw_messages").await?, 1);
    Ok(())
}

async fn fetch_str(
    conn: &(impl QueryExecutor + ?Sized),
    sql: &str,
    params: impl IntoParams,
) -> Result<String, String> {
    let mut rows = conn.query(sql, params).await.map_err(|e| e.to_string())?;
    let row = rows
        .next()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no row".to_string())?;
    row.get::<String>(0).map_err(|e| e.to_string())
}
