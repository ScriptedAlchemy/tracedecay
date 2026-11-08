use tracedecay_domain::{ComponentVersion, SanitizationReceiptV1};
use tracedecay_privacy::{
    LCM_PAYLOAD_SANITIZER_VERSION_V1, sanitize_lcm_payload_text, verify_sanitized_json_payload,
};
use tracedecay_runtime_core::db::engine::{TestConnection, TransactionBehavior};
use tracedecay_store::{StoredCanonicalBody, UPSERT_CANONICAL_BODY_SQL};

use super::*;

struct Store {
    conn: TestConnection,
    _directory: tempfile::TempDir,
}

impl Store {
    async fn open() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let conn = TestConnection::open(&directory.path().join("replay-metadata.db"));
        conn.execute_batch(
            "CREATE TABLE sessions (
                provider TEXT NOT NULL, session_id TEXT NOT NULL,
                PRIMARY KEY(provider, session_id)
             ); INSERT INTO sessions VALUES ('codex', 'session');",
        )
        .await
        .unwrap();
        schema::ensure_lcm_schema(&conn).await.unwrap();
        Self {
            conn,
            _directory: directory,
        }
    }

    async fn insert(&self, ordinal: i64, text: &str, replay: Value, canonical: bool) -> i64 {
        let sanitized = sanitize_lcm_payload_text(text).unwrap();
        assert_eq!(sanitized.sanitized_text(), text);
        let body = StoredCanonicalBody::pack(text.as_bytes()).unwrap();
        let metadata = serde_json::json!({
            "lcm_active_replay": true,
            "active_replay": {"role": "assistant", "content": replay, "provider_field": "kept"},
            "ingest_protection": {"sanitization_receipt": sanitized.receipt()}
        });
        let metadata =
            raw::resanitize_stored_provider_metadata_json(&metadata.to_string()).unwrap();
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        if canonical {
            transaction
                .execute(
                    UPSERT_CANONICAL_BODY_SQL,
                    params![
                        body.content_hash.as_str(),
                        body.encoding,
                        body.blob.as_slice(),
                        body.uncompressed_bytes
                    ],
                )
                .await
                .unwrap();
        }
        transaction
            .execute(
                "INSERT INTO lcm_raw_messages(provider, message_id, session_id, role, ordinal,
                timestamp, content, content_hash, storage_kind, metadata_json)
             VALUES ('codex', ?1, 'session', 'assistant', ?2, NULL, ?3, ?4, 'inline', ?5)",
                params![
                    format!("message-{ordinal}"),
                    ordinal,
                    (!canonical).then_some(text),
                    body.content_hash.as_str(),
                    metadata.as_str()
                ],
            )
            .await
            .unwrap();
        let mut rows = transaction
            .query(
                "SELECT store_id FROM lcm_raw_messages WHERE ordinal = ?1",
                params![ordinal],
            )
            .await
            .unwrap();
        let id = rows.next().await.unwrap().unwrap().get(0).unwrap();
        transaction.commit().await.unwrap();
        id
    }

    async fn tick(&self, rows: usize) -> ReplayMetadataCompactionReport {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        let report = step(&transaction, rows).await.unwrap();
        transaction.commit().await.unwrap();
        report
    }
}

async fn scalar(conn: &(impl QueryExecutor + ?Sized), sql: &str) -> i64 {
    conn.query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}

fn metadata(message: &LcmRawMessage) -> Value {
    serde_json::from_str(message.metadata_json.as_deref().unwrap()).unwrap()
}

#[asupersync::test]
async fn removes_only_the_duplicate_and_preserves_receipts_replay_and_summary_state() {
    let store = Store::open().await;
    let text = "retained verified reasoning and tool evidence ".repeat(180);
    let id = store
        .insert(1, &text, Value::String(text.clone()), true)
        .await;
    store
        .conn
        .execute_batch(
            "INSERT INTO session_summary_nodes(summary_id, provider, conversation_id, session_id,
             depth, summary_text, summary_hash, summary_token_count, source_token_count)
         VALUES ('summary', 'codex', 'conversation', 'session', 0, 'summary', 'hash', 1, 1);
         INSERT INTO session_summary_sources(summary_id, source_kind, source_id, ordinal)
         VALUES ('summary', 'raw_message', '1', 0);
         UPDATE lcm_summary_convergence_queue SET state = 'current', attempted_raw_store_id = 1;",
        )
        .await
        .unwrap();
    let before = raw::load_raw_message_by_store_id(&store.conn, id)
        .await
        .unwrap();
    let before_replay = crate::replay_transactions::raw_replay_message(&before);
    let revision = scalar(
        &store.conn,
        "SELECT raw_revision_generation FROM lcm_summary_convergence_queue",
    )
    .await;
    let report = store.tick(48).await;
    assert_eq!(report.rows_compacted, 1);
    assert!(report.metadata_bytes_removed >= text.len() as u64);
    assert!(!report.has_more);
    let after = raw::load_raw_message_by_store_id(&store.conn, id)
        .await
        .unwrap();
    assert_eq!(after.content, before.content);
    assert_eq!(after.content_hash, before.content_hash);
    assert_eq!(
        crate::replay_transactions::raw_replay_message(&after),
        before_replay
    );
    let mut after_metadata = metadata(&after);
    assert!(after_metadata["active_replay"].get("content").is_none());
    assert_eq!(
        after_metadata["ingest_protection"]["sanitization_receipt"],
        metadata(&before)["ingest_protection"]["sanitization_receipt"]
    );
    let protection = after_metadata
        .as_object_mut()
        .unwrap()
        .remove("ingest_protection")
        .unwrap();
    let receipt: SanitizationReceiptV1 =
        serde_json::from_value(protection["metadata_sanitization_receipt"].clone()).unwrap();
    verify_sanitized_json_payload(
        &Value::String(after_metadata.to_string()),
        &receipt,
        &ComponentVersion::new(LCM_PAYLOAD_SANITIZER_VERSION_V1).unwrap(),
    )
    .unwrap();
    assert_eq!(
        scalar(
            &store.conn,
            "SELECT raw_revision_generation FROM lcm_summary_convergence_queue"
        )
        .await,
        revision
    );
    assert_eq!(
        scalar(
            &store.conn,
            "SELECT COUNT(*) FROM lcm_summary_convergence_dirty_raw"
        )
        .await,
        0
    );
    assert_eq!(
        scalar(&store.conn, "SELECT COUNT(*) FROM session_summary_sources").await,
        1
    );
    assert_eq!(
        scalar(&store.conn, "SELECT COUNT(*) FROM session_canonical_bodies").await,
        1
    );
    summary_convergence::validate_raw_update_guard(&store.conn)
        .await
        .unwrap();
    store.conn.execute("UPDATE lcm_raw_messages SET metadata_json = json_set(metadata_json, '$.provider_note', 'changed') WHERE store_id = ?1", params![id]).await.unwrap();
    assert_eq!(
        scalar(
            &store.conn,
            "SELECT COUNT(*) FROM lcm_summary_convergence_dirty_raw"
        )
        .await,
        1,
        "ordinary updates still invalidate"
    );
}

#[asupersync::test]
async fn structured_different_media_and_external_representations_are_retained() {
    let store = Store::open().await;
    let text = "ordinary original body ".repeat(30);
    store
        .insert(
            1,
            &text,
            serde_json::json!([{"type": "text", "text": text}]),
            false,
        )
        .await;
    store
        .insert(
            2,
            &text,
            Value::String("provider-specific replay representation".repeat(30)),
            false,
        )
        .await;
    let placeholder = "[externalized tool output: retained-payload-placeholder]".repeat(20);
    store
        .insert(3, &placeholder, Value::String(placeholder.clone()), false)
        .await;
    let id = store
        .insert(4, &text, Value::String(text.clone()), false)
        .await;
    store.conn.execute("UPDATE lcm_raw_messages SET storage_kind = 'external', payload_ref = 'payload-ref', placeholder_text = 'external' WHERE store_id = ?1", params![id]).await.unwrap();
    let media = "data:image/png;base64,aGVsbG8=";
    store
        .insert(5, media, Value::String(media.to_owned()), false)
        .await;
    let report = store.tick(48).await;
    assert_eq!(report.rows_scanned, 5);
    assert_eq!(report.rows_compacted, 0);
    assert_eq!(report.metadata_bytes_removed, 0);
    // All five provider representations still carry their original content.
    assert_eq!(scalar(&store.conn, "SELECT COUNT(*) FROM lcm_raw_messages WHERE json_type(metadata_json, '$.active_replay.content') IS NOT NULL").await, 5);
}

#[asupersync::test]
async fn corrupt_body_rolls_back_prior_changes_frontier_and_guard_suspension() {
    let store = Store::open().await;
    let first = "first verified duplicated string ".repeat(120);
    let second = "second verified duplicated string ".repeat(160);
    let id = store
        .insert(1, &first, Value::String(first.clone()), false)
        .await;
    store
        .insert(2, &second, Value::String(second.clone()), true)
        .await;
    let original = raw::load_raw_message_by_store_id(&store.conn, id)
        .await
        .unwrap();
    store
        .conn
        .execute_batch("UPDATE session_canonical_bodies SET body = X'00'")
        .await
        .unwrap();
    let transaction = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    assert!(step(&transaction, 48).await.is_err());
    transaction.rollback().await.unwrap();
    assert_eq!(frontier(&store.conn).await.unwrap(), 0);
    assert_eq!(
        raw::load_raw_message_by_store_id(&store.conn, id)
            .await
            .unwrap(),
        original
    );
    summary_convergence::validate_raw_update_guard(&store.conn)
        .await
        .unwrap();
    store.conn.execute("UPDATE lcm_raw_messages SET metadata_json = json_set(metadata_json, '$.provider_note', 'changed') WHERE store_id = ?1", params![id]).await.unwrap();
    assert_eq!(
        scalar(
            &store.conn,
            "SELECT COUNT(*) FROM lcm_summary_convergence_dirty_raw"
        )
        .await,
        1,
        "rollback restores ordinary invalidation"
    );
}

#[asupersync::test]
async fn stale_metadata_cannot_be_overwritten_and_missing_guard_refuses_repair() {
    let store = Store::open().await;
    let text = "verified duplicate content ".repeat(80);
    let id = store
        .insert(1, &text, Value::String(text.clone()), false)
        .await;
    let snapshot = raw::load_raw_message_by_store_id(&store.conn, id)
        .await
        .unwrap();
    let compacted = compacted_metadata(&snapshot).unwrap().unwrap();
    store.conn.execute("UPDATE lcm_raw_messages SET metadata_json = json_set(metadata_json, '$.provider_note', 'concurrent') WHERE store_id = ?1", params![id]).await.unwrap();
    assert!(
        !apply_metadata(&store.conn, id, &snapshot, &compacted)
            .await
            .unwrap()
    );
    assert_eq!(
        metadata(
            &raw::load_raw_message_by_store_id(&store.conn, id)
                .await
                .unwrap()
        )["provider_note"],
        "concurrent"
    );
    store
        .conn
        .execute_batch("DROP TRIGGER lcm_summary_convergence_raw_unprotected_update")
        .await
        .unwrap();
    let transaction = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    let error = step(&transaction, 48).await.unwrap_err();
    assert!(error.to_string().contains("guard is missing"));
    transaction.rollback().await.unwrap();
    assert_eq!(frontier(&store.conn).await.unwrap(), 0);
}

#[asupersync::test]
async fn row_and_byte_budgets_advance_a_durable_frontier_and_finished_pages_are_noops() {
    let store = Store::open().await;
    let text = "bounded replay text ".repeat(35_000);
    for ordinal in 1..=4 {
        store
            .insert(ordinal, &text, Value::String(text.clone()), true)
            .await;
    }
    let first = store.tick(1).await;
    assert_eq!(first.rows_scanned, 1);
    assert_eq!(first.rows_compacted, 1);
    assert!(first.has_more);
    assert_eq!(frontier(&store.conn).await.unwrap(), 1);
    let second = store.tick(48).await;
    assert!(
        second.rows_scanned > 0 && second.rows_scanned < 3,
        "byte cap stops before the third large row"
    );
    assert!(second.has_more);
    let last = store.tick(48).await;
    assert!(!last.has_more);
    let idle = store.tick(48).await;
    assert_eq!(idle, ReplayMetadataCompactionReport::default());
}

#[asupersync::test]
async fn zero_compaction_pages_advance_distinct_durable_frontiers() {
    let store = Store::open().await;
    for ordinal in 1..=52 {
        store
            .insert(
                ordinal,
                "canonical source",
                Value::String("different replay".to_owned()),
                false,
            )
            .await;
    }
    let first = store.tick(48).await;
    assert_eq!(first.rows_scanned, 48);
    assert_eq!(first.rows_compacted, 0);
    assert_eq!(first.frontier_before, Some(0));
    assert_eq!(first.frontier_after, Some(48));
    assert!(first.has_more);

    let second = store.tick(48).await;
    assert_eq!(second.rows_scanned, 4);
    assert_eq!(second.rows_compacted, 0);
    assert_eq!(second.frontier_before, Some(48));
    assert_eq!(second.frontier_after, Some(52));
    assert!(!second.has_more);
    assert_eq!(frontier(&store.conn).await.unwrap(), 52);

    assert_eq!(
        store.tick(48).await,
        ReplayMetadataCompactionReport::default()
    );
}

#[asupersync::test]
async fn large_ineligible_metadata_is_bounded_before_eligibility_checks() {
    let store = Store::open().await;
    let structured = serde_json::json!([{
        "type": "text",
        "text": "retained structured replay ".repeat(20_000)
    }]);
    let mut originals = Vec::new();
    for ordinal in 1..=9 {
        let id = store
            .insert(ordinal, "small verified body", structured.clone(), false)
            .await;
        originals.push((
            id,
            raw::load_raw_message_by_store_id(&store.conn, id)
                .await
                .unwrap(),
        ));
    }
    let total_metadata_bytes: u64 = originals
        .iter()
        .map(|(_, message)| message.metadata_json.as_ref().unwrap().len() as u64)
        .sum();
    assert!(total_metadata_bytes > MAX_BATCH_BYTES);
    assert!(originals.iter().all(|(_, message)| {
        message.metadata_json.as_ref().unwrap().len() < 1024 * 1024
    }));

    let mut after = 0;
    let mut examined = 0;
    let mut scanned = 0;
    let mut ticks = 0;
    for _ in 0..originals.len() {
        let report = store.tick(MAX_REPLAY_METADATA_COMPACTION_ROWS).await;
        ticks += 1;
        assert!(report.rows_scanned > 0);
        assert_eq!(report.rows_compacted, 0);
        assert_eq!(report.rows_skipped, 0);
        assert_eq!(report.metadata_bytes_removed, 0);
        assert!(report.metadata_bytes_examined > 0);
        assert!(report.metadata_bytes_examined <= MAX_BATCH_BYTES);
        assert_eq!(report.frontier_before, Some(after));
        let next = report.frontier_after.unwrap();
        assert!(next > after);
        assert_eq!(frontier(&store.conn).await.unwrap(), next);
        after = next;
        examined += report.metadata_bytes_examined;
        scanned += report.rows_scanned;
        if !report.has_more {
            break;
        }
    }
    assert!(ticks > 1, "ineligible JSON must obey the byte budget too");
    assert_eq!(scanned, originals.len() as u64);
    assert_eq!(examined, total_metadata_bytes, "no document is revisited");
    assert_eq!(after, originals.last().unwrap().0);
    assert_eq!(
        store.tick(MAX_REPLAY_METADATA_COMPACTION_ROWS).await,
        ReplayMetadataCompactionReport::default()
    );
    for (id, original) in originals {
        assert_eq!(
            raw::load_raw_message_by_store_id(&store.conn, id)
                .await
                .unwrap(),
            original,
            "bounded scanning must preserve the body, replay and receipts"
        );
    }
}

#[asupersync::test]
async fn oversized_metadata_is_preserved_without_blocking_later_compaction() {
    let store = Store::open().await;
    let text = "verified duplicate content ".repeat(80);
    let oversized_id = store
        .insert(1, &text, Value::String(text.clone()), false)
        .await;
    let next_id = store
        .insert(2, &text, Value::String(text.clone()), false)
        .await;
    // Bypass ingress only to exercise a pre-existing oversized stored document.
    // The repair must not parse or rewrite it, nor pin the durable frontier.
    let oversized = serde_json::json!({
        "lcm_active_replay": true,
        "active_replay": {"content": text},
        "padding": "x".repeat(MAX_BATCH_BYTES as usize)
    })
    .to_string();
    store
        .conn
        .execute(
            "UPDATE lcm_raw_messages SET metadata_json = ?1 WHERE store_id = ?2",
            params![oversized.as_str(), oversized_id],
        )
        .await
        .unwrap();
    let report = store.tick(MAX_REPLAY_METADATA_COMPACTION_ROWS).await;
    assert_eq!(report.rows_scanned, 2);
    assert_eq!(report.rows_skipped, 1);
    assert_eq!(report.rows_skipped_oversized, 1);
    assert_eq!(report.rows_compacted, 1);
    assert!(report.metadata_bytes_examined > 0);
    assert!(report.metadata_bytes_examined < MAX_BATCH_BYTES);
    assert_eq!(report.frontier_after, Some(next_id));
    assert_eq!(frontier(&store.conn).await.unwrap(), next_id);
    assert!(!report.has_more);
    let mut rows = store
        .conn
        .query(
            "SELECT metadata_json FROM lcm_raw_messages WHERE store_id = ?1",
            params![oversized_id],
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<String>(0).unwrap(),
        oversized
    );
    drop(rows);
    assert_eq!(
        store.tick(MAX_REPLAY_METADATA_COMPACTION_ROWS).await,
        ReplayMetadataCompactionReport::default()
    );
    summary_convergence::validate_raw_update_guard(&store.conn)
        .await
        .unwrap();
}
