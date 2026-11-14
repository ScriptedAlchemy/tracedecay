//! Isolate process-wide read accounting from concurrently running unit tests.

use std::process::Command;

use tracedecay_privacy::sanitize_lcm_payload_text;
use tracedecay_runtime_core::db::engine::{
    Executor, QueryExecutor, TestConnection, TransactionBehavior, params,
};

use super::{MAX_BATCH_BYTES, ReplayMetadataCompactionReport, frontier, step};
use crate::{raw, retrieval_content::projected_content_hash, schema, summary_convergence};

fn read_chars() -> u64 {
    std::fs::read_to_string("/proc/self/io")
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("rchar:"))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[tokio::test]
async fn oversized_rows_skip_payload_io() {
    const CHILD: &str = "TRACEDECAY_REPLAY_READ_COST_CHILD";
    const TEST: &str = "retention::replay_metadata::read_cost::oversized_rows_skip_payload_io";
    if std::env::var_os(CHILD).is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--test-threads=1"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "isolated read-cost test failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("1 passed; 0 failed"), "{stdout}");
        return;
    }

    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("replay-read-cost.db");
    let conn = TestConnection::open(&database_path);
    conn.execute_batch(
        "CREATE TABLE sessions (
            provider TEXT NOT NULL, session_id TEXT NOT NULL,
            PRIMARY KEY(provider, session_id)
         ); INSERT INTO sessions VALUES ('codex', 'session');",
    )
    .await
    .unwrap();
    schema::ensure_lcm_schema(&conn).await.unwrap();

    let small = "verified replay after oversized rows ".repeat(80);
    let sanitized = sanitize_lcm_payload_text(&small).unwrap();
    let metadata = serde_json::json!({
        "lcm_active_replay": true,
        "active_replay": {"role": "assistant", "content": small},
        "ingest_protection": {"sanitization_receipt": sanitized.receipt()}
    });
    let metadata = raw::resanitize_stored_provider_metadata_json(&metadata.to_string()).unwrap();
    let large = "x".repeat(2 * MAX_BATCH_BYTES as usize);
    let padded = format!("{metadata}{}", " ".repeat(2 * MAX_BATCH_BYTES as usize));
    let large_hash = projected_content_hash(&large);
    let small_hash = projected_content_hash(&small);
    conn.execute(
        "INSERT INTO session_canonical_bodies(content_hash, encoding, body, uncompressed_bytes)
         VALUES (?1, 'identity', ?2, ?3)",
        params![large_hash.as_str(), large.as_bytes(), large.len() as i64],
    )
    .await
    .unwrap();
    // Direct fixture writes represent oversized retained data, not new ingress.
    // Each overflow location must be rejected before its bytes are loaded:
    // inline body, metadata, then a separately stored canonical body.
    let fixtures = [
        (
            1_i64,
            Some(large.as_str()),
            large_hash.as_str(),
            metadata.as_str(),
        ),
        (
            2,
            Some(small.as_str()),
            small_hash.as_str(),
            padded.as_str(),
        ),
        (3, None, large_hash.as_str(), metadata.as_str()),
        (
            4,
            Some(small.as_str()),
            small_hash.as_str(),
            metadata.as_str(),
        ),
    ];
    for (id, content, hash, document) in fixtures {
        conn.execute(
            "INSERT INTO lcm_raw_messages(provider, message_id, session_id, role, ordinal,
                content, content_hash, storage_kind, metadata_json)
             VALUES ('codex', ?1, 'session', 'assistant', ?2, ?3, ?4, 'inline', ?5)",
            params![format!("message-{id}"), id, content, hash, document],
        )
        .await
        .unwrap();
    }
    let original = raw::load_raw_message_by_store_id(&conn, 4).await.unwrap();
    drop(conn);

    for id in 1..=3 {
        // Fresh connections preserve the oversized-read measurement without
        // issuing PRAGMAs forbidden by the exact-SQL test authority.
        let conn = TestConnection::open(&database_path);
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        let before = read_chars();
        let report = step(&transaction, 1).await.unwrap();
        transaction.commit().await.unwrap();
        let read = read_chars() - before;
        assert!(
            read < MAX_BATCH_BYTES,
            "rejected row {id} read {read} bytes before admission; budget={MAX_BATCH_BYTES}"
        );
        assert_eq!(report.rows_scanned, 1);
        assert_eq!(report.rows_skipped_oversized, 1);
        assert_eq!(report.rows_compacted, 0);
        assert_eq!(report.metadata_bytes_examined, 0);
        assert_eq!(report.frontier_before, Some(id - 1));
        assert_eq!(report.frontier_after, Some(id));
        assert_eq!(frontier(&conn).await.unwrap(), id);
        assert!(report.has_more);
    }

    let conn = TestConnection::open(&database_path);
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    let report = step(&transaction, 48).await.unwrap();
    transaction.commit().await.unwrap();
    assert_eq!(report.rows_scanned, 1);
    assert_eq!(report.rows_compacted, 1);
    assert!(report.metadata_bytes_removed > 0);
    assert_eq!(report.frontier_after, Some(4));
    assert!(!report.has_more);
    let repaired = raw::load_raw_message_by_store_id(&conn, 4).await.unwrap();
    assert_eq!(repaired.content, original.content);
    assert_eq!(
        crate::replay_transactions::raw_replay_message(&repaired),
        crate::replay_transactions::raw_replay_message(&original)
    );
    summary_convergence::validate_raw_update_guard(&conn)
        .await
        .unwrap();

    // Check complete retained bytes outside the measured interval.
    for (id, content, _, document) in &fixtures[..3] {
        let mut rows = conn
            .query(
                "SELECT content, metadata_json FROM lcm_raw_messages WHERE store_id = ?1",
                params![*id],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<Option<String>>(0).unwrap().as_deref(), *content);
        assert_eq!(row.get::<String>(1).unwrap(), *document);
    }
    let mut rows = conn
        .query("SELECT body FROM session_canonical_bodies", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<Vec<u8>>(0)
            .unwrap(),
        large.as_bytes()
    );
    drop(rows);
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    assert_eq!(
        step(&transaction, 48).await.unwrap(),
        ReplayMetadataCompactionReport::default()
    );
    transaction.commit().await.unwrap();
}
