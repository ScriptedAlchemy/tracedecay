use std::path::PathBuf;

use crate::{LcmSourceRef, dag, schema};
use tracedecay_domain::HydrationStateV1;
use tracedecay_domain::canonical_text::sha256_hex;
use tracedecay_privacy::sanitize_lcm_payload_text;
use tracedecay_runtime_core::db::engine::{
    Connection, Executor, IntoParams, QueryExecutor, TestConnection, params,
};

use super::*;

const PROVIDER: &str = "cursor";
const SESSION: &str = "session-a";
const DAY: i64 = 24 * 60 * 60;
const NOW: i64 = 1_900_000_000;
/// Summary text every fixture summary node carries; the node stores this text's
/// real content hash so expansion's integrity check passes.
const SUMMARY_TEXT: &str = "summary";

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
        );",
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
    crate::test_support::seed_active_generation(&runtime, SESSION).await;
    Ok(TestStore {
        conn,
        _runtime: runtime,
        storage_root,
        _temp: temp,
    })
}

/// Inserts an inline message row with the given age. Returns the assigned `store_id`. The row carries a real
/// ingest sanitization receipt so verified reads (summary expansion) accept it.
async fn insert_message(
    conn: &(impl Executor + ?Sized),
    ordinal: i64,
    age_days: i64,
    content: &str,
) -> Result<i64, String> {
    let message_id = format!("msg-{ordinal}");
    let timestamp = NOW - age_days * DAY;
    let sanitization =
        sanitize_lcm_payload_text(content).map_err(|err| format!("sanitize: {err}"))?;
    let content = sanitization.sanitized_text();
    let hash = sha256_hex(content.as_bytes());
    let metadata = serde_json::json!({
        "ingest_protection": { "sanitization_receipt": sanitization.receipt() }
    })
    .to_string();
    conn.execute(
        "INSERT INTO lcm_raw_messages (
            provider, message_id, session_id, role, ordinal, timestamp,
            content, content_hash, storage_kind, payload_ref, metadata_json
         )
         VALUES (?1, ?2, ?3, 'assistant', ?4, ?5, ?6, ?7, 'inline', NULL, ?8)",
        params![
            PROVIDER,
            message_id.as_str(),
            SESSION,
            ordinal,
            timestamp,
            content,
            hash.as_str(),
            metadata.as_str()
        ],
    )
    .await
    .map_err(|err| format!("insert raw: {err}"))?;
    let store_id = fetch_i64(
        conn,
        "SELECT store_id FROM lcm_raw_messages WHERE provider = ?1 AND message_id = ?2",
        params![PROVIDER, message_id.as_str()],
    )
    .await?;
    Ok(store_id)
}

/// Marks a raw row projection-durable by adding a summary node whose lineage
/// covers `store_id`. Returns the summary node id so callers can expand it.
async fn make_projection_durable(
    conn: &(impl Executor + ?Sized),
    store_id: i64,
) -> Result<String, String> {
    let node_id = format!("node-{store_id}");
    let summary_hash = crate::retrieval_content::projected_content_hash(SUMMARY_TEXT);
    conn.execute(
        "INSERT INTO session_summary_nodes(
            summary_id, provider, conversation_id, session_id, depth, summary_text,
            summary_hash, summary_token_count, source_token_count
         )
         VALUES (?1, ?2, 'conv', ?3, 0, ?4, ?5, 1, 1)",
        params![
            node_id.as_str(),
            PROVIDER,
            SESSION,
            SUMMARY_TEXT,
            summary_hash.as_str()
        ],
    )
    .await
    .map_err(|err| format!("insert summary node: {err}"))?;
    conn.execute(
        "INSERT INTO session_summary_sources(summary_id, source_kind, source_id, ordinal)
         VALUES (?1, 'raw_message', ?2, 0)",
        params![node_id.as_str(), store_id.to_string()],
    )
    .await
    .map_err(|err| format!("insert summary source: {err}"))?;
    conn.execute(
        "INSERT INTO session_summary_availability(session_id, generation, summary_id, availability)
         VALUES (?1, ?2, ?3, 'available')",
        params![
            SESSION,
            crate::test_support::FIXTURE_GENERATION,
            node_id.as_str()
        ],
    )
    .await
    .map_err(|err| format!("insert summary availability: {err}"))?;
    Ok(node_id)
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

#[asupersync::test]
async fn authority_loss_before_commit_rolls_back_retention_mutations() -> Result<(), String> {
    let store = test_store().await?;
    let durable = insert_message(&store.conn, 1, 90, "must survive").await?;
    make_projection_durable(&store.conn, durable).await?;
    let scope = std::sync::Mutex::new(Some(
        tracedecay_runtime_core::db::enter_daemon_database_scope(
            &store.storage_root,
            1,
            "session-retention-revocation-test",
        )
        .map_err(|error| error.to_string())?,
    ));
    let authority = tracedecay_runtime_core::db::DatabaseAuthority::for_runtime(
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
    Ok(())
}

// Dropping projection-durable raw rows is intentional (plan 38 §3: the summary
// is the durable survivor), so summary expansion must degrade rather than
// abort once the drop window passes. The dropped source is reported as
// retention-expired (plan 23 hydration state); it is never confused with an
// ownership violation, which would make every summary older than the window
// unreadable under a misleading error.
#[asupersync::test]
async fn expansion_survives_sources_dropped_by_retention() -> Result<(), String> {
    let store = test_store().await?;
    let conn = &store.conn;
    let durable = insert_message(conn, 1, 90, "durable old content").await?;
    let node_id = make_projection_durable(conn, durable).await?;

    let before = dag::expand_summary_node(conn, PROVIDER, SESSION, &node_id)
        .await
        .map_err(|error| format!("expansion before retention: {error}"))?;
    assert_eq!(before.sources.len(), 1);
    assert_eq!(before.sources[0].state, HydrationStateV1::Available);
    assert_eq!(before.sources[0].content, "durable old content");

    let report = run_apply(conn, &store.storage_root, &drop_config(30)).await?;
    assert_eq!(
        report.dropped.acted, 1,
        "the summary's source row is dropped"
    );
    assert_eq!(count(conn, "lcm_raw_messages").await?, 0);
    assert_eq!(
        count(conn, "session_summary_sources").await?,
        1,
        "lineage still points at the dropped store_id"
    );

    let after = dag::expand_summary_node(conn, PROVIDER, SESSION, &node_id)
        .await
        .map_err(|error| format!("expansion must survive retention: {error}"))?;

    assert_eq!(
        after.summary.node_id, node_id,
        "the summary itself survives"
    );
    assert_eq!(after.summary.summary_text, SUMMARY_TEXT);
    assert_eq!(
        after.sources.len(),
        1,
        "the retention-dropped source is still reported, not silently elided"
    );
    assert_eq!(
        after.sources[0].source_ref,
        LcmSourceRef::RawMessage { store_id: durable }
    );
    assert_eq!(
        after.sources[0].state,
        HydrationStateV1::RetentionExpired,
        "dropped source is reported unavailable, not as a session-ownership error"
    );
    assert!(after.sources[0].content.is_empty());
    assert!(after.sources[0].raw_message.is_none());
    assert!(after.sources[0].raw_message_metadata.is_none());
    Ok(())
}

// A raw row that is present but owned by a different session is still a hard
// ownership violation: degrading on absence must not weaken that boundary.
#[asupersync::test]
async fn expansion_still_rejects_foreign_session_sources() -> Result<(), String> {
    let store = test_store().await?;
    let conn = &store.conn;
    let store_id = insert_message(conn, 1, 90, "foreign content").await?;
    let node_id = make_projection_durable(conn, store_id).await?;
    conn.execute(
        "INSERT INTO sessions(provider, session_id, project_key, project_path)
         VALUES (?1, 'session-b', '/p', '/p')",
        params![PROVIDER],
    )
    .await
    .map_err(|err| format!("insert foreign session: {err}"))?;
    conn.execute(
        "UPDATE lcm_raw_messages SET session_id = 'session-b' WHERE store_id = ?1",
        params![store_id],
    )
    .await
    .map_err(|err| format!("reassign raw row: {err}"))?;

    let error = dag::expand_summary_node(conn, PROVIDER, SESSION, &node_id)
        .await
        .expect_err("a present but foreign source must not be disclosed");

    assert!(
        matches!(error, LcmError::SummarySourceNotOwnedBySession),
        "unexpected error: {error:?}"
    );
    Ok(())
}

// (b) retention window is honored: a projection-durable row inside the window
// is not dropped.
#[asupersync::test]
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
#[asupersync::test]
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

#[asupersync::test]
async fn backlog_read_reports_real_eligible_bytes_and_watermark() -> Result<(), String> {
    let store = test_store().await?;
    let content = "retention backlog café ☕";
    let durable = insert_message(&store.conn, 1, 90, content).await?;
    make_projection_durable(&store.conn, durable).await?;

    let records = read_session_retention_backlog(
        &store.conn,
        tracedecay_contracts::storage::StoreKeyV1::new("sessions.db")
            .map_err(|error| error.to_string())?,
        &drop_config(30),
        NOW,
    )
    .await
    .map_err(|error| error.to_string())?;

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].table.as_str(), "lcm_raw_messages");
    assert_eq!(records[0].past_window_bytes.get(), content.len() as u64);
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

#[asupersync::test]
async fn backlog_read_emits_clean_zero_record_for_configured_window() -> Result<(), String> {
    let store = test_store().await?;
    let records = read_session_retention_backlog(
        &store.conn,
        tracedecay_contracts::storage::StoreKeyV1::new("sessions.db")
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

// One content copy: a durable row's body is stored once, and dropping the row
// removes its FTS entries with it.
#[asupersync::test]
async fn drop_removes_the_single_content_copy_and_its_index() -> Result<(), String> {
    let store = test_store().await?;
    let conn = &store.conn;
    let store_id = insert_message(conn, 1, 90, "stored once").await?;
    make_projection_durable(conn, store_id).await?;
    let live = insert_message(conn, 2, 90, "stored once too").await?;
    let indexed = |conn: &Connection| {
        let conn = conn.clone();
        async move {
            fetch_i64(
                &conn,
                "SELECT COUNT(*) FROM lcm_raw_messages_fts WHERE lcm_raw_messages_fts MATCH 'stored'",
                (),
            )
            .await
        }
    };
    assert_eq!(indexed(conn).await?, 2);

    let report = run_apply(conn, &store.storage_root, &drop_config(30)).await?;

    assert_eq!(report.dropped.acted, 1);
    assert_eq!(count(conn, "lcm_raw_messages").await?, 1);
    assert_eq!(
        fetch_i64(conn, "SELECT store_id FROM lcm_raw_messages", ()).await?,
        live,
        "the row without durable lineage is retained"
    );
    assert_eq!(
        indexed(conn).await?,
        1,
        "the dropped row left the FTS index"
    );
    Ok(())
}

// Compaction only changes projection-durable rows, and retains complete text
// through the canonical verified reader without creating a plain payload file.
#[asupersync::test]
async fn offload_compacts_durable_content_without_creating_payload_files() -> Result<(), String> {
    let store = test_store().await?;
    let conn = &store.conn;
    let content = "x".repeat(8192);
    let durable = insert_message(conn, 1, 90, &content).await?;
    let live = insert_message(conn, 2, 90, &content).await?;
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
    assert_eq!(report.canonicalized_rows, 1);
    assert!(report.errors.is_empty(), "{:?}", report.errors);

    // The ordinary inline reader hydrates the shared canonical body.
    let kind: String = fetch_str(
        conn,
        "SELECT storage_kind FROM lcm_raw_messages WHERE store_id = ?1",
        params![durable],
    )
    .await?;
    assert_eq!(kind, "inline");
    let payload_present = fetch_i64(conn, "SELECT COUNT(*) FROM lcm_external_payloads", ()).await?;
    assert_eq!(payload_present, 0);
    assert_eq!(count(conn, "session_canonical_bodies").await?, 1);
    assert!(!payload::payload_dir(&store.storage_root).exists());
    let restored = raw::load_raw_message_by_store_id(conn, durable)
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(restored.content, content);
    assert!(restored.payload_ref.is_none());
    let live_content = fetch_str(
        conn,
        "SELECT content FROM lcm_raw_messages WHERE store_id = ?1",
        params![live],
    )
    .await?;
    assert_eq!(live_content, content, "live un-projected row stays intact");
    let repeated = run_apply(conn, &store.storage_root, &config).await?;
    assert_eq!(repeated.offloaded.acted, 0);
    assert_eq!(repeated.canonicalized_rows, 0);
    Ok(())
}

#[asupersync::test]
async fn offload_keeps_small_bodies_inline_and_uses_byte_threshold() -> Result<(), String> {
    let store = test_store().await?;
    let small = "é".repeat(INLINE_BODY_BYTES / 2 - 1);
    let small_id = insert_message(&store.conn, 1, 90, &small).await?;
    make_projection_durable(&store.conn, small_id).await?;
    let config = LcmRetentionConfig {
        drop_after_days: None,
        ..LcmRetentionConfig::default()
    };
    let report = run_apply(&store.conn, &store.storage_root, &config).await?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.offloaded.eligible, 0);
    assert_eq!(count(&store.conn, "lcm_external_payloads").await?, 0);
    assert!(!payload::payload_dir(&store.storage_root).exists());
    let backlog = read_session_retention_backlog(
        &store.conn,
        tracedecay_contracts::storage::StoreKeyV1::new("sessions.db")
            .map_err(|error| error.to_string())?,
        &config,
        NOW,
    )
    .await
    .map_err(|error| error.to_string())?;
    assert_eq!(backlog[0].past_window_bytes.get(), 0);

    let boundary = "é".repeat(INLINE_BODY_BYTES / 2);
    let boundary_id = insert_message(&store.conn, 2, 90, &boundary).await?;
    make_projection_durable(&store.conn, boundary_id).await?;
    let report = run_apply(&store.conn, &store.storage_root, &config).await?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.offloaded.eligible, 1);
    assert_eq!(
        report.offloaded.acted, 0,
        "the full UTF-8 snippet would duplicate this body"
    );
    assert_eq!(report.offloaded.bytes_reclaimed, 0);
    assert_eq!(count(&store.conn, "lcm_external_payloads").await?, 0);
    let unchanged = raw::load_raw_message_by_store_id(&store.conn, small_id)
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(unchanged.content, small);
    Ok(())
}

#[asupersync::test]
async fn small_offload_restores_verified_text_and_releases_file_only_after_gc_grace()
-> Result<(), String> {
    let store = test_store().await?;
    let content = "small tool output: café completed";
    let target = seed_retention_offload(&store, 1, content).await?;
    let path = payload::payload_dir(&store.storage_root).join(&target.payload_ref);
    let config = LcmRetentionConfig {
        drop_after_days: None,
        ..LcmRetentionConfig::default()
    };
    let preview = run_session_retention_authorized(
        &store.conn,
        &store.storage_root,
        "all",
        None,
        &config,
        RetentionMode::DryRun,
        NOW,
        &|_| Ok(()),
    )
    .await
    .map_err(|error| error.to_string())?;
    assert_eq!(preview.inlined.eligible, 1);
    assert_eq!(preview.inlined.acted, 0);
    assert_eq!(preview.bytes_reclaimed(), 0);
    assert_eq!(count(&store.conn, "lcm_external_payloads").await?, 1);
    assert!(path.exists());

    let report = run_apply(&store.conn, &store.storage_root, &config).await?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.inlined.acted, 1);
    assert_eq!(report.offloaded.acted, 0);
    assert_eq!(report.bytes_reclaimed(), 0);
    assert_eq!(count(&store.conn, "session_canonical_bodies").await?, 0);
    let restored = raw::load_raw_message_by_store_id(&store.conn, target.store_id)
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(restored.content, content);
    assert_eq!(restored.content_hash, target.content_hash);
    assert_eq!(restored.metadata_json, target.metadata_json);
    assert!(restored.payload_ref.is_none());
    assert!(path.exists(), "inlining never deletes the payload file");
    assert_eq!(
        fetch_i64(
            &store.conn,
            "SELECT COUNT(*) FROM lcm_raw_messages_fts
             WHERE lcm_raw_messages_fts MATCH 'completed'",
            (),
        )
        .await?,
        1,
        "restoration also makes the original tool text searchable again"
    );

    let marked_at = fetch_i64(
        &store.conn,
        "SELECT first_seen_at FROM lcm_gc_marks WHERE payload_ref = ?1",
        params![target.payload_ref.as_str()],
    )
    .await?;
    let gc_config = crate::LcmGcConfig::default();
    for (now, retained) in [
        (marked_at + gc_config.grace_seconds as i64 - 1, true),
        (marked_at + gc_config.grace_seconds as i64 + 1, false),
    ] {
        let gc = crate::gc::run_payload_gc_with_apply(
            &store.conn,
            &store.storage_root,
            PROVIDER,
            Some(SESSION),
            &gc_config,
            true,
            now,
        )
        .await
        .map_err(|error| error.to_string())?;
        assert!(gc.errors.is_empty(), "{:?}", gc.errors);
        assert_eq!(path.exists(), retained);
        assert_eq!(
            count(&store.conn, "lcm_external_payloads").await?,
            i64::from(retained)
        );
    }
    let restored = raw::load_raw_message_by_store_id(&store.conn, target.store_id)
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(restored.content, content, "full text survives payload GC");
    Ok(())
}

#[asupersync::test]
async fn offload_restoration_rejects_corruption_staleness_and_lost_authority() -> Result<(), String>
{
    for failure in ["payload", "receipt", "stale", "metadata", "authority"] {
        let store = test_store().await?;
        let content = if failure == "payload" {
            "original complete text".to_owned()
        } else {
            "original complete text ".repeat(1024)
        };
        let mut target = seed_retention_offload(&store, 1, &content).await?;
        let path = payload::payload_dir(&store.storage_root).join(&target.payload_ref);
        match failure {
            "payload" => {
                std::fs::write(&path, "tampered bytes").map_err(|error| error.to_string())?
            }
            "receipt" => {
                store
                    .conn
                    .execute(
                        "UPDATE lcm_raw_messages SET metadata_json = '{}' WHERE store_id = ?1",
                        params![target.store_id],
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                target.metadata_json = Some("{}".to_owned());
            }
            "stale" => {
                store
                    .conn
                    .execute(
                        "UPDATE lcm_raw_messages SET timestamp = ?2 WHERE store_id = ?1",
                        params![target.store_id, NOW],
                    )
                    .await
                    .map_err(|error| error.to_string())?;
            }
            "metadata" => {
                store
                    .conn
                    .execute(
                        "UPDATE lcm_raw_messages
                     SET metadata_json = json_set(metadata_json, '$.changed', 1)
                     WHERE store_id = ?1",
                        params![target.store_id],
                    )
                    .await
                    .map_err(|error| error.to_string())?;
            }
            _ => {}
        }
        let convergence_before = mark_convergence_current(&store.conn).await?;
        let error = inline_offloaded_one(
            RetentionStore::Connection(&store.conn),
            &store.storage_root,
            &target,
            Some(&|intent| {
                if failure == "authority" && intent == "commit session retention inline pass" {
                    Err(LcmError::Cancelled)
                } else {
                    Ok(())
                }
            }),
        )
        .await
        .expect_err("unproven or changed payload must remain external");
        match failure {
            "payload" | "receipt" => assert_eq!(error, LcmError::PayloadIntegrityMismatch),
            "stale" | "metadata" => {
                assert!(error.to_string().contains("compare-and-swap rejected"))
            }
            "authority" => assert_eq!(error, LcmError::Cancelled),
            _ => unreachable!(),
        }
        assert_eq!(
            fetch_i64(
                &store.conn,
                "SELECT COUNT(*) FROM lcm_raw_messages
                 WHERE store_id = ?1 AND storage_kind = 'external'
                   AND content IS NULL AND payload_ref = ?2",
                params![target.store_id, target.payload_ref.as_str()],
            )
            .await?,
            1,
            "{failure} must preserve the original row"
        );
        assert_eq!(count(&store.conn, "lcm_external_payloads").await?, 1);
        assert_eq!(count(&store.conn, "session_canonical_bodies").await?, 0);
        assert!(path.exists(), "{failure} must preserve the payload file");
        assert_eq!(convergence_snapshot(&store.conn).await?, convergence_before);
        summary_convergence::validate_raw_update_guard(&store.conn)
            .await
            .map_err(|error| error.to_string())?;
        store
            .conn
            .execute(
                "UPDATE lcm_raw_messages SET timestamp = timestamp + 1 WHERE store_id = ?1",
                params![target.store_id],
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            count(&store.conn, "lcm_summary_convergence_dirty_raw").await?,
            1,
            "ordinary evidence changes still invalidate after {failure}"
        );
    }
    Ok(())
}

#[asupersync::test]
async fn large_offloads_share_verified_canonical_text_and_release_files_after_grace()
-> Result<(), String> {
    let store = test_store().await?;
    let content = "restored searchable café tool output\n".repeat(1024);
    let first = seed_retention_offload(&store, 1, &content).await?;
    let second = seed_retention_offload(&store, 2, &content).await?;
    let convergence_before = mark_convergence_current(&store.conn).await?;
    let config = LcmRetentionConfig {
        drop_after_days: None,
        ..LcmRetentionConfig::default()
    };
    let report = run_apply(&store.conn, &store.storage_root, &config).await?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.inlined.acted, 2);
    assert_eq!(report.canonicalized_rows, 2);
    assert_eq!(report.offloaded.acted, 0);
    assert_eq!(
        report.bytes_reclaimed(),
        0,
        "restoration does not report moved bytes as reclaimed"
    );
    assert_eq!(count(&store.conn, "session_canonical_bodies").await?, 1);
    assert_eq!(convergence_snapshot(&store.conn).await?, convergence_before);
    assert_eq!(fetch_i64(&store.conn,
        "SELECT COUNT(*) FROM lcm_raw_messages_fts WHERE lcm_raw_messages_fts MATCH 'searchable'", ()
    ).await?, 2);
    for target in [&first, &second] {
        let restored = raw::load_raw_message_by_store_id(&store.conn, target.store_id)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(restored.content, content);
        assert_eq!(restored.content_hash, target.content_hash);
        assert_eq!(restored.metadata_json, target.metadata_json);
        assert!(restored.payload_ref.is_none());
        assert!(
            payload::payload_dir(&store.storage_root)
                .join(&target.payload_ref)
                .exists()
        );
    }
    let marked_at = fetch_i64(
        &store.conn,
        "SELECT MAX(first_seen_at) FROM lcm_gc_marks",
        (),
    )
    .await?;
    let gc_config = crate::LcmGcConfig::default();
    let gc = crate::gc::run_payload_gc_with_apply(
        &store.conn,
        &store.storage_root,
        PROVIDER,
        Some(SESSION),
        &gc_config,
        true,
        marked_at + gc_config.grace_seconds as i64 + 1,
    )
    .await
    .map_err(|error| error.to_string())?;
    assert!(gc.errors.is_empty(), "{:?}", gc.errors);
    assert_eq!(count(&store.conn, "lcm_external_payloads").await?, 0);
    for target in [&first, &second] {
        assert!(
            !payload::payload_dir(&store.storage_root)
                .join(&target.payload_ref)
                .exists()
        );
        assert_eq!(
            raw::load_raw_message_by_store_id(&store.conn, target.store_id)
                .await
                .map_err(|error| error.to_string())?
                .content,
            content
        );
    }
    summary_convergence::validate_raw_update_guard(&store.conn)
        .await
        .map_err(|error| error.to_string())?;
    store.conn.execute(
        "UPDATE lcm_raw_messages SET metadata_json = json_set(metadata_json, '$.provider_note', 'changed')
         WHERE store_id = ?1", params![first.store_id],
    ).await.map_err(|error| error.to_string())?;
    assert_eq!(
        count(&store.conn, "lcm_summary_convergence_dirty_raw").await?,
        1,
        "ordinary metadata changes still invalidate after successful restoration"
    );
    Ok(())
}

#[asupersync::test]
async fn canonical_compaction_counts_utf8_snippets_and_marginal_shared_body_cost()
-> Result<(), String> {
    let store = test_store().await?;
    let mut state = 0x923a_405b_u64;
    let ascii: String = (0..6000)
        .map(|index| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            if index % 9 == 8 {
                ' '
            } else {
                char::from(b'a' + ((state >> 32) % 26) as u8)
            }
        })
        .collect();
    let unicode = "é".repeat(3000);
    let mut targets = Vec::new();
    for (index, content) in [&ascii, &unicode].into_iter().enumerate() {
        let packed = pack_canonical(content).map_err(|error| error.to_string())?;
        let snippet = raw::derived_text_for_snippet(content);
        assert!(
            packed.blob.len() < content.len(),
            "BLOB-only comparison would permit this body"
        );
        assert!(
            packed.blob.len() + snippet.len() >= content.len(),
            "the full representation grows"
        );
        targets.push(seed_retention_offload(&store, index as i64 + 1, content).await?);
        let direct = insert_message(&store.conn, index as i64 + 3, 90, content).await?;
        make_projection_durable(&store.conn, direct).await?;
    }
    assert!(
        raw::derived_text_for_snippet(&unicode).len()
            > raw::derived_text_for_snippet(&unicode).chars().count()
    );
    let config = LcmRetentionConfig {
        drop_after_days: None,
        ..LcmRetentionConfig::default()
    };
    let report = run_apply(&store.conn, &store.storage_root, &config).await?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.inlined.eligible, 2);
    assert_eq!(report.offloaded.eligible, 2);
    assert_eq!(report.inlined.acted + report.offloaded.acted, 0);
    assert_eq!(count(&store.conn, "session_canonical_bodies").await?, 0);
    assert_eq!(count(&store.conn, "lcm_external_payloads").await?, 2);

    // Another ordinary canonical owner makes this BLOB a zero-marginal-cost
    // reuse. Only its new snippet and record allowance need to fit.
    let packed = pack_canonical(&ascii).map_err(|error| error.to_string())?;
    let txn = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|error| error.to_string())?;
    raw::persist_canonical_body(&txn, &packed)
        .await
        .map_err(|error| error.to_string())?;
    txn.execute(
        "UPDATE lcm_raw_messages SET content = NULL, placeholder_text = ?1 WHERE message_id = 'msg-3'",
        params![raw::derived_text_for_snippet(&ascii)],
    ).await.map_err(|error| error.to_string())?;
    txn.commit().await.map_err(|error| error.to_string())?;
    assert_eq!(
        inline_offloaded_one(
            RetentionStore::Connection(&store.conn),
            &store.storage_root,
            &targets[0],
            None
        )
        .await
        .map_err(|error| error.to_string())?,
        Some(true)
    );
    let restored = raw::load_raw_message_by_store_id(&store.conn, targets[0].store_id)
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(restored.content, ascii);
    assert_eq!(restored.metadata_json, targets[0].metadata_json);
    assert_eq!(count(&store.conn, "session_canonical_bodies").await?, 1);
    assert_eq!(
        fetch_str(
            &store.conn,
            "SELECT storage_kind FROM lcm_raw_messages WHERE store_id = ?1",
            params![targets[1].store_id]
        )
        .await?,
        "external"
    );
    Ok(())
}

#[asupersync::test]
async fn restoration_revisits_old_cursor_and_stops_before_exceeding_byte_budget()
-> Result<(), String> {
    let store = test_store().await?;
    let content = "bounded historical body\n".repeat(3 * 1024 * 1024 / 24);
    let mut targets = Vec::new();
    for ordinal in 1..=3 {
        targets.push(seed_retention_offload(&store, ordinal, &content).await?);
    }
    let oversized =
        seed_retention_offload(&store, 4, &"x".repeat(MAX_REPACK_BODY_BYTES + 1)).await?;
    RetentionCursor {
        aged_through: (NOW, i64::MAX),
        durable_through: i64::MAX,
    }
    .persist(&store.conn, "inline_offloaded")
    .await
    .map_err(|error| error.to_string())?;
    let config = LcmRetentionConfig {
        drop_after_days: None,
        ..LcmRetentionConfig::default()
    };
    let preview = run_session_retention_authorized(
        &store.conn,
        &store.storage_root,
        "all",
        None,
        &config,
        RetentionMode::DryRun,
        NOW,
        &|_| Ok(()),
    )
    .await
    .map_err(|error| error.to_string())?;
    assert_eq!(preview.inlined.eligible, 2);
    assert_eq!(preview.inlined.acted, 0);
    assert_eq!(count(&store.conn, "session_canonical_bodies").await?, 0);
    assert!(
        schema::get_gc_meta(&store.conn, &RetentionCursor::keys(RESTORE_CURSOR).0)
            .await
            .map_err(|error| error.to_string())?
            .is_none()
    );
    for (expected, has_more) in [(2, true), (1, false), (0, false)] {
        let report = run_apply(&store.conn, &store.storage_root, &config).await?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.inlined.acted, expected);
        assert_eq!(report.canonicalized_rows, expected);
        assert_eq!(report.storage_has_more, has_more);
    }
    for target in &targets {
        assert_eq!(
            raw::load_raw_message_by_store_id(&store.conn, target.store_id)
                .await
                .map_err(|error| error.to_string())?
                .content,
            content
        );
    }
    assert_eq!(
        fetch_str(
            &store.conn,
            "SELECT payload_ref FROM lcm_raw_messages WHERE store_id = ?1",
            params![oversized.store_id]
        )
        .await?,
        oversized.payload_ref
    );
    assert!(
        payload::payload_dir(&store.storage_root)
            .join(&oversized.payload_ref)
            .exists()
    );
    // A short continuation also drains newly durable inline rows across its
    // row cap without repeating already-compacted work or skipping a source.
    let direct_content = "short continuation text\n".repeat(1024);
    for ordinal in 5..=7 {
        let store_id = insert_message(&store.conn, ordinal, 90, &direct_content).await?;
        make_projection_durable(&store.conn, store_id).await?;
    }
    let config = LcmRetentionConfig {
        max_batch_size: 1,
        ..config
    };
    let mut total = 0;
    let mut drained = false;
    for _ in 0..6 {
        let report = run_apply(&store.conn, &store.storage_root, &config).await?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(report.offloaded.acted <= 1);
        total += report.offloaded.acted;
        if !report.storage_has_more {
            drained = true;
            break;
        }
    }
    assert!(drained, "bounded short continuations must reach the end");
    assert_eq!(total, 3);
    Ok(())
}

#[asupersync::test]
async fn canonical_compaction_commit_denial_preserves_original_inline_body() -> Result<(), String> {
    let store = test_store().await?;
    let content = "complete retained body\n".repeat(1024);
    let store_id = insert_message(&store.conn, 1, 90, &content).await?;
    make_projection_durable(&store.conn, store_id).await?;
    let original = raw::load_raw_message_by_store_id(&store.conn, store_id)
        .await
        .map_err(|error| error.to_string())?;
    let config = LcmRetentionConfig {
        drop_after_days: None,
        ..LcmRetentionConfig::default()
    };
    let report = run_session_retention_authorized(
        &store.conn,
        &store.storage_root,
        "all",
        None,
        &config,
        RetentionMode::Apply,
        NOW,
        &|intent| {
            if intent == "commit session retention canonical pass" {
                Err(LcmError::Cancelled)
            } else {
                Ok(())
            }
        },
    )
    .await
    .map_err(|error| error.to_string())?;
    assert_eq!(report.errors.len(), 1);
    assert_eq!(report.offloaded.acted, 0);
    assert!(
        !report.storage_has_more,
        "authority loss must not request a hot-loop retry"
    );
    let retained = raw::load_raw_message_by_store_id(&store.conn, store_id)
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(retained.content, original.content);
    assert_eq!(retained.metadata_json, original.metadata_json);
    assert_eq!(count(&store.conn, "session_canonical_bodies").await?, 0);
    assert_eq!(count(&store.conn, "lcm_external_payloads").await?, 0);
    assert_eq!(
        RetentionCursor::read(&store.conn, "offload")
            .await
            .map_err(|error| error.to_string())?
            .aged_through,
        (i64::MIN, i64::MIN)
    );
    Ok(())
}

#[asupersync::test]
async fn corrupt_shared_body_cannot_replace_plaintext_or_external_source() -> Result<(), String> {
    let store = test_store().await?;
    let content = "verified source text\n".repeat(1024);
    let external = seed_retention_offload(&store, 1, &content).await?;
    let direct = insert_message(&store.conn, 2, 90, &content).await?;
    make_projection_durable(&store.conn, direct).await?;
    let mut packed = pack_canonical(&content).map_err(|error| error.to_string())?;
    packed.blob = vec![0xff];
    raw::persist_canonical_body(&store.conn, &packed)
        .await
        .map_err(|error| error.to_string())?;
    // Both kinds have independent authoritative content and must not fetch an
    // unrelated canonical BLOB. The normal canonical path still rejects it.
    for store_id in [external.store_id, direct] {
        let mut rows = store
            .conn
            .query(
                &format!(
                    "SELECT {} FROM lcm_raw_messages WHERE store_id = ?1",
                    raw::RAW_MESSAGE_SELECT_COLUMNS
                ),
                params![store_id],
            )
            .await
            .map_err(|error| error.to_string())?;
        let row = rows
            .next()
            .await
            .map_err(|error| error.to_string())?
            .ok_or("missing fixture row")?;
        assert!(
            row.get::<Option<Vec<u8>>>(14)
                .map_err(|error| error.to_string())?
                .is_none()
        );
        raw::verified_raw_message_from_row(&row).map_err(|error| error.to_string())?;
    }
    let config = LcmRetentionConfig {
        drop_after_days: None,
        ..LcmRetentionConfig::default()
    };
    let report = run_apply(&store.conn, &store.storage_root, &config).await?;
    assert_eq!(report.errors.len(), 2);
    assert_eq!(report.inlined.acted + report.offloaded.acted, 0);
    assert_eq!(
        fetch_str(
            &store.conn,
            "SELECT content FROM lcm_raw_messages WHERE store_id = ?1",
            params![direct]
        )
        .await?,
        content
    );
    assert_eq!(
        fetch_str(
            &store.conn,
            "SELECT payload_ref FROM lcm_raw_messages WHERE store_id = ?1",
            params![external.store_id]
        )
        .await?,
        external.payload_ref
    );
    assert!(
        payload::payload_dir(&store.storage_root)
            .join(&external.payload_ref)
            .exists()
    );
    assert_eq!(
        RetentionCursor::read(&store.conn, RESTORE_CURSOR)
            .await
            .map_err(|error| error.to_string())?
            .aged_through,
        (i64::MIN, i64::MIN)
    );
    store
        .conn
        .execute(
            "UPDATE lcm_raw_messages SET content = NULL, placeholder_text = ?1 WHERE store_id = ?2",
            params![raw::derived_text_for_snippet(&content), direct],
        )
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(
        raw::load_raw_message_by_store_id(&store.conn, direct)
            .await
            .expect_err("actual canonical content must be verified"),
        LcmError::PayloadIntegrityMismatch
    );
    Ok(())
}

#[asupersync::test]
async fn small_offload_restoration_is_bounded_scoped_and_leaves_protected_payloads()
-> Result<(), String> {
    let store = test_store().await?;
    let first = seed_retention_offload(&store, 1, "first old body").await?;
    let second = seed_retention_offload(&store, 2, "second old body").await?;
    let protected = seed_retention_offload(&store, 3, "protected body").await?;
    store
        .conn
        .execute(
            "UPDATE lcm_external_payloads SET kind = 'quarantined_assistant_output'
             WHERE payload_ref = ?1",
            params![protected.payload_ref.as_str()],
        )
        .await
        .map_err(|error| error.to_string())?;
    let config = LcmRetentionConfig {
        drop_after_days: None,
        max_batch_size: 1,
        ..LcmRetentionConfig::default()
    };
    for disabled in [
        LcmRetentionConfig {
            enabled: false,
            ..config
        },
        LcmRetentionConfig {
            offload_after_days: None,
            ..config
        },
    ] {
        let report = run_apply(&store.conn, &store.storage_root, &disabled).await?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.inlined.eligible, 0);
        assert_eq!(report.inlined.acted, 0);
    }
    for (provider, session_id, expected) in [
        ("other-provider", None, 0),
        (PROVIDER, Some("other-session"), 0),
        (PROVIDER, Some(SESSION), 1),
        ("all", None, 1),
        ("all", None, 0),
    ] {
        let report = run_session_retention_authorized(
            &store.conn,
            &store.storage_root,
            provider,
            session_id,
            &config,
            RetentionMode::Apply,
            NOW,
            &|_| Ok(()),
        )
        .await
        .map_err(|error| error.to_string())?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.inlined.acted, expected);
        assert!(report.inlined.eligible <= 1);
        assert_eq!(report.bytes_reclaimed(), 0);
    }
    for target in [&first, &second] {
        assert!(
            raw::load_raw_message_by_store_id(&store.conn, target.store_id)
                .await
                .map_err(|error| error.to_string())?
                .payload_ref
                .is_none()
        );
    }
    assert_eq!(
        fetch_str(
            &store.conn,
            "SELECT payload_ref FROM lcm_raw_messages WHERE store_id = ?1",
            params![protected.store_id],
        )
        .await?,
        protected.payload_ref
    );
    assert!(
        payload::payload_dir(&store.storage_root)
            .join(&protected.payload_ref)
            .exists()
    );
    Ok(())
}

#[asupersync::test]
async fn offload_cas_preserves_revived_row_and_rolls_back_canonical_body() -> Result<(), String> {
    let store = test_store().await?;
    let original = "stale content".repeat(1024);
    let store_id = insert_message(&store.conn, 1, 90, &original).await?;
    make_projection_durable(&store.conn, store_id).await?;
    let metadata_json = raw::load_raw_message_by_store_id(&store.conn, store_id)
        .await
        .map_err(|error| error.to_string())?
        .metadata_json;
    let target = OffloadRow {
        store_id,
        provider: PROVIDER.to_string(),
        session_id: SESSION.to_string(),
        message_id: "msg-1".to_string(),
        timestamp: NOW - 90 * DAY,
        content: original,
        metadata_json,
    };
    let revived = "revived content";
    let revived_hash = sha256_hex(revived.as_bytes());
    store
        .conn
        .execute(
            "UPDATE lcm_raw_messages
             SET timestamp = ?2, content = ?3, content_hash = ?4
             WHERE store_id = ?1",
            params![store_id, NOW, revived, revived_hash.as_str()],
        )
        .await
        .map_err(|error| error.to_string())?;

    fn authorize(_: &str) -> Result<(), LcmError> {
        Ok(())
    }
    let error = offload_one(
        RetentionStore::Connection(&store.conn),
        &target,
        Some(&authorize),
    )
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
    assert_eq!(count(&store.conn, "session_canonical_bodies").await?, 0);
    assert!(!payload::payload_dir(&store.storage_root).exists());
    Ok(())
}

/// A pass resumes from the previous one's cursors, yet still drops a durable
/// row that aged past the window since, and an old row a summary first
/// covered since.
#[asupersync::test]
async fn later_passes_act_on_rows_that_aged_or_became_durable_since() -> Result<(), String> {
    let store = test_store().await?;
    let conn = &store.conn;
    let ages_later = insert_message(conn, 1, 20, "durable, ages past the window later").await?;
    make_projection_durable(conn, ages_later).await?;
    let summarized_later = insert_message(conn, 2, 90, "old, summarized later").await?;
    let young = insert_message(conn, 3, 1, "durable and young").await?;
    make_projection_durable(conn, young).await?;
    let config = LcmRetentionConfig {
        offload_after_days: None,
        ..drop_config(30)
    };
    let pass = |now: i64| {
        run_session_retention_authorized(
            conn,
            &store.storage_root,
            "all",
            None,
            &config,
            RetentionMode::Apply,
            now,
            &|_| Ok(()),
        )
    };

    let first = pass(NOW).await.map_err(|error| error.to_string())?;
    assert_eq!(first.dropped.acted, 0);

    make_projection_durable(conn, summarized_later).await?;
    let second = pass(NOW + 15 * DAY)
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(second.dropped.acted, 2);
    let mut rows = conn
        .query(
            "SELECT store_id FROM lcm_raw_messages ORDER BY store_id",
            (),
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut remaining = Vec::new();
    while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
        remaining.push(row.get::<i64>(0).map_err(|error| error.to_string())?);
    }
    assert_eq!(remaining, [young]);
    Ok(())
}

// (e) reclaimed space is measurable via page/free-list metrics.
#[asupersync::test]
async fn reports_measurable_reclaim_metrics() -> Result<(), String> {
    let store = test_store().await?;
    let conn = &store.conn;
    for ordinal in 1..=8 {
        let store_id = insert_message(conn, ordinal, 90, &"y".repeat(2048)).await?;
        make_projection_durable(conn, store_id).await?;
    }
    let report = run_apply(conn, &store.storage_root, &drop_config(30)).await?;

    assert_eq!(report.dropped.acted, 8);
    assert_eq!(count(conn, "lcm_raw_messages").await?, 0);
    assert!(report.page_count_before > 0, "page_count observed");
    assert!(
        report.freelist_after >= report.freelist_before,
        "deleted rows freed pages"
    );
    assert!(report.bytes_reclaimed() >= 8 * 2048);
    Ok(())
}

// Disabled config is an inert no-op even in Apply mode.
#[asupersync::test]
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

async fn convergence_snapshot(conn: &(impl QueryExecutor + ?Sized)) -> Result<String, String> {
    fetch_str(
        conn,
        "SELECT json_array(state, protection_frontier_store_id, attempted_raw_store_id,
                           raw_revision_generation, stale_from_store_id,
                           (SELECT COUNT(*) FROM lcm_summary_convergence_dirty_raw),
                           (SELECT COUNT(*) FROM lcm_summary_convergence_invalidation_work),
                           (SELECT COUNT(*) FROM session_summary_sources))
         FROM lcm_summary_convergence_queue",
        (),
    )
    .await
}

async fn mark_convergence_current(conn: &(impl Executor + ?Sized)) -> Result<String, String> {
    conn.execute_batch(
        "DELETE FROM lcm_summary_convergence_invalidation_work;
         DELETE FROM lcm_summary_convergence_dirty_raw;
         UPDATE lcm_summary_convergence_queue SET state = 'current',
            attempted_raw_store_id = newest_raw_store_id,
            protection_frontier_store_id = newest_raw_store_id,
            stale_from_store_id = NULL;",
    )
    .await
    .map_err(|error| error.to_string())?;
    convergence_snapshot(conn).await
}

/// Seeds the prior retention representation through its actual payload writer.
async fn seed_retention_offload(
    store: &TestStore,
    ordinal: i64,
    content: &str,
) -> Result<InlineOffloadedRow, String> {
    let store_id = insert_message(&store.conn, ordinal, 90, content).await?;
    make_projection_durable(&store.conn, store_id).await?;
    let original = raw::load_raw_message_by_store_id(&store.conn, store_id)
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(
        original.content, content,
        "fixture content must survive normal sanitization"
    );
    offload_external_one(
        RetentionStore::Connection(&store.conn),
        &store.storage_root,
        &OffloadRow {
            store_id,
            provider: PROVIDER.to_owned(),
            session_id: SESSION.to_owned(),
            message_id: format!("msg-{ordinal}"),
            timestamp: NOW - 90 * DAY,
            content: content.to_owned(),
            metadata_json: original.metadata_json,
        },
        None,
    )
    .await
    .map_err(|error| error.to_string())?;
    let mut rows = store
        .conn
        .query(
            "SELECT payload_ref, content_hash, placeholder_text, metadata_json
             FROM lcm_raw_messages WHERE store_id = ?1",
            params![store_id],
        )
        .await
        .map_err(|error| error.to_string())?;
    let row = rows
        .next()
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "missing offloaded row".to_string())?;
    Ok(InlineOffloadedRow {
        store_id,
        timestamp: NOW - 90 * DAY,
        provider: PROVIDER.to_owned(),
        session_id: SESSION.to_owned(),
        message_id: format!("msg-{ordinal}"),
        payload_ref: row.get(0).map_err(|error| error.to_string())?,
        content_hash: row.get(1).map_err(|error| error.to_string())?,
        placeholder_text: row.get(2).map_err(|error| error.to_string())?,
        metadata_json: row.get(3).map_err(|error| error.to_string())?,
        byte_count: content.len(),
    })
}
